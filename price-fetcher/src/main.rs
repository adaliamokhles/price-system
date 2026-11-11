use std::thread::sleep;
use std::time::Duration;

// socketcan: CanSocket = connection to a CAN bus, CanFrame = one CAN message,
// Socket = gives us write_frame(), EmbeddedFrame = gives us CanFrame::new(),
// StandardId = a normal 11-bit CAN ID (0x000 to 0x7FF)
use socketcan::{CanFrame, CanSocket, EmbeddedFrame, Socket, StandardId};

// The 6 pairs. Their position in this list decides their CAN ID:
// index 0 → 0x100, index 1 → 0x101, ... index 5 → 0x105
const PAIRS: [&str; 6] = ["BTC-USD", "ETH-USD", "SOL-USD", "XRP-USD", "ADA-USD", "DOGE-USD"];
const BASE_ID: u16 = 0x100;   // u16 = whole number from 0 to 65535

// Same function as before: get one price from Coinbase
fn fetch_price(pair: &str) -> Result<f64, Box<dyn std::error::Error>> {
    let url = format!("https://api.coinbase.com/v2/prices/{}/spot", pair);
    let body = ureq::get(&url).call()?.into_string()?;
    let json: serde_json::Value = serde_json::from_str(&body)?;
    let amount = json["data"]["amount"]
        .as_str()
        .ok_or("no amount in answer")?
        .parse::<f64>()?;
    Ok(amount)
}

// Send one price on the CAN bus with the given ID
fn send_price(sock: &CanSocket, id: u16, price: f64) -> Result<(), Box<dyn std::error::Error>> {
    // Turn the price into exactly 8 bytes (little-endian)
    let data = price.to_le_bytes();

    // Build the CAN ID. StandardId::new returns None if the number is too big,
    // ok_or turns that "None" into an error
    let can_id = StandardId::new(id).ok_or("invalid CAN ID")?;

    // Build the CAN frame: ID + 8 data bytes
    let frame = CanFrame::new(can_id, &data).ok_or("could not build CAN frame")?;

    // Put it on the bus
    sock.write_frame(&frame)?;
    Ok(())
}

fn main() {
    // Open the CAN bus "vcan0". If it fails (e.g. vcan0 doesn't exist), stop with a clear message.
    let sock = CanSocket::open("vcan0").expect("could not open vcan0 (does it exist and is it up?)");
    println!("price-fetcher started: sending 6 prices on vcan0 every 5 s (Ctrl+C to stop)");

    // loop = repeat forever
    loop {
        // enumerate() gives each item AND its position: (0, "BTC-USD"), (1, "ETH-USD"), ...
        for (index, pair) in PAIRS.iter().enumerate() {
            let id = BASE_ID + index as u16;               // 0x100 + position
            let coin = pair.split('-').next().unwrap_or(pair);

            match fetch_price(pair) {
                Ok(price) => match send_price(&sock, id, price) {
                    Ok(()) => println!("sent  0x{:03X}  {:<4} = {:.4}", id, coin, price),
                    Err(e) => println!("CAN error for {}: {}", coin, e),
                },
                Err(e) => println!("fetch error for {}: {}", coin, e),
            }
        }
        println!("---");
        sleep(Duration::from_secs(5));
    }
}
