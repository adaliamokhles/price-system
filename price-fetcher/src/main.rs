use std::sync::{Arc, Mutex};                 // shared "coins OK" number between the 2 workers
use std::thread::{self, sleep};              // thread = second worker, sleep = wait
use std::time::Duration;

// socketcan: CanSocket = bus connection, CanFrame = one CAN message,
// Socket = gives write_frame(), EmbeddedFrame = gives CanFrame::new(),
// StandardId = a normal 11-bit CAN ID
use socketcan::{CanFrame, CanSocket, EmbeddedFrame, Socket, StandardId};

// The 6 pairs. Position in the list = CAN ID: 0 -> 0x100 ... 5 -> 0x105
const PAIRS: [&str; 6] = ["BTC-USD", "ETH-USD", "SOL-USD", "XRP-USD", "ADA-USD", "DOGE-USD"];
const BASE_ID: u16 = 0x100;

// Gateway heartbeat: "I'm alive" message, sent every second
// Data: [alive counter 0..255, number of coins fetched OK in the last round 0..6]
const HEARTBEAT_ID: u16 = 0x0F0;

// Get one price from Coinbase (gives up after 4 seconds)
fn fetch_price(pair: &str) -> Result<f64, Box<dyn std::error::Error>> {
    let url = format!("https://api.coinbase.com/v2/prices/{}/spot", pair);
    let body = ureq::get(&url)
        .timeout(Duration::from_secs(4))
        .call()?
        .into_string()?;
    let json: serde_json::Value = serde_json::from_str(&body)?;
    let amount = json["data"]["amount"]
        .as_str()
        .ok_or("no amount in answer")?
        .parse::<f64>()?;
    Ok(amount)
}

// Send one price on the CAN bus: 8 bytes (f64, little-endian)
fn send_price(sock: &CanSocket, id: u16, price: f64) -> Result<(), Box<dyn std::error::Error>> {
    let data = price.to_le_bytes();
    let can_id = StandardId::new(id).ok_or("invalid CAN ID")?;
    let frame = CanFrame::new(can_id, &data).ok_or("could not build CAN frame")?;
    sock.write_frame(&frame)?;
    Ok(())
}

// WORKER 2: send the heartbeat every second, forever
fn heartbeat_loop(sock: CanSocket, coins_ok: Arc<Mutex<u8>>) {
    let mut counter: u8 = 0;
    loop {
        // Read the latest "coins OK" number (lock, copy the value, unlock)
        let ok = *coins_ok.lock().unwrap();

        // Build and send the 2-byte heartbeat frame [counter, coins OK]
        let data = [counter, ok];
        if let Some(id) = StandardId::new(HEARTBEAT_ID) {
            if let Some(frame) = CanFrame::new(id, &data) {
                if let Err(e) = sock.write_frame(&frame) {
                    eprintln!("heartbeat CAN error: {}", e);
                }
            }
        }

        // Next counter value: 0, 1, ... 255, then back to 0
        counter = counter.wrapping_add(1);
        sleep(Duration::from_secs(1));
    }
}

fn main() {
    // Two connections to the bus: one for prices (this worker), one for the heartbeat worker
    let sock = CanSocket::open("vcan0").expect("could not open vcan0 (does it exist and is it up?)");
    let hb_sock = CanSocket::open("vcan0").expect("could not open vcan0 for the heartbeat");

    // Shared "coins OK" number, 0 until the first round is done
    let coins_ok = Arc::new(Mutex::new(0u8));

    // Start worker 2 (heartbeat) with its own handle to the shared number
    let coins_ok_for_hb = Arc::clone(&coins_ok);
    thread::spawn(move || heartbeat_loop(hb_sock, coins_ok_for_hb));

    println!("price-fetcher started: 6 prices every 5 s + heartbeat 0x0F0 every 1 s (Ctrl+C to stop)");

    // WORKER 1 (this main thread): fetch and send the prices, forever
    loop {
        let mut ok_count: u8 = 0;

        for (index, pair) in PAIRS.iter().enumerate() {
            let id = BASE_ID + index as u16;
            let coin = pair.split('-').next().unwrap_or(pair);

            match fetch_price(pair) {
                Ok(price) => match send_price(&sock, id, price) {
                    Ok(()) => {
                        ok_count += 1;
                        println!("sent  0x{:03X}  {:<4} = {:.4}", id, coin, price);
                    }
                    Err(e) => println!("CAN error for {}: {}", coin, e),
                },
                Err(e) => println!("fetch error for {}: {}", coin, e),
            }
        }

        // Tell the heartbeat worker how this round went
        *coins_ok.lock().unwrap() = ok_count;
        println!("--- {}/6 coins OK", ok_count);

        sleep(Duration::from_secs(5));
    }
}
