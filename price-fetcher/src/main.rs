use std::sync::{Arc, Mutex};                 // shared "OK counts" between the workers
use std::thread::{self, sleep};              // thread = extra worker, sleep = wait
use std::time::Duration;

use socketcan::{CanFrame, CanSocket, EmbeddedFrame, Socket, StandardId};

// ---------------- CAN ID map ----------------
// 0x0F0          gateway heartbeat  [counter, crypto OK 0..6, markets OK 0..10]
// 0x100..0x1FF   crypto (6 used: 0x100..0x105)
// 0x200..0x2FF   markets (10 used: 0x200..0x209)
const HEARTBEAT_ID: u16 = 0x0F0;
const CRYPTO_BASE_ID: u16 = 0x100;
const MARKETS_BASE_ID: u16 = 0x200;

// ---------------- Crypto (as before) ----------------
const PAIRS: [&str; 6] = ["BTC-USD", "ETH-USD", "SOL-USD", "XRP-USD", "ADA-USD", "DOGE-USD"];

// ---------------- Markets ----------------
// Where each market price comes from, and how to read it:
//   CoinbaseInvert("EUR"): Coinbase gives "EUR per 1 USD"  -> we show 1 / rate  (EUR/USD)
//   CoinbaseDirect("JPY"): Coinbase gives "JPY per 1 USD"  -> we show the rate  (USD/JPY)
//   XausGold / XausSilver: xaus.com gives USD per ounce    -> we show it as is
#[derive(Clone, Copy)]
enum Src {
    CoinbaseInvert(&'static str),
    CoinbaseDirect(&'static str),
    XausGold,
    XausSilver,
}

// The 10 instruments. Position in this list = CAN ID: 0 -> 0x200 ... 9 -> 0x209
const MARKETS: [(&str, Src); 10] = [
    ("EUR/USD", Src::CoinbaseInvert("EUR")),
    ("GBP/USD", Src::CoinbaseInvert("GBP")),
    ("AUD/USD", Src::CoinbaseInvert("AUD")),
    ("NZD/USD", Src::CoinbaseInvert("NZD")),
    ("USD/JPY", Src::CoinbaseDirect("JPY")),
    ("USD/CHF", Src::CoinbaseDirect("CHF")),
    ("USD/CAD", Src::CoinbaseDirect("CAD")),
    ("XAU/USD", Src::XausGold),
    ("XAG/USD", Src::XausSilver),
    ("BTC/USD", Src::CoinbaseInvert("BTC")),
];
const MARKETS_INTERVAL_S: u64 = 20;
const COINBASE_RATES_URL: &str = "https://api.coinbase.com/v2/exchange-rates?currency=USD";
const XAUS_SPOT_URL: &str = "https://xaus.com/api/v1/spot?compact=1";

// Download a web address and read it as JSON (gives up after 6 seconds)
fn fetch_json(url: &str) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let body = ureq::get(url)
        .timeout(Duration::from_secs(6))
        .call()?
        .into_string()?;
    Ok(serde_json::from_str(&body)?)
}

// Get one crypto price from Coinbase (gives up after 4 seconds)
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

// One currency rate from Coinbase's answer: {"data":{"rates":{"EUR":"0.9067", ...}}}
fn coinbase_rate(json: &serde_json::Value, code: &str) -> Option<f64> {
    json["data"]["rates"][code].as_str()?.parse::<f64>().ok()
}

// Turn "units per 1 USD" into the usual quote (flip it, or keep it).
// Rejects zero, negative or broken numbers.
fn rate_to_quote(rate: f64, invert: bool) -> Option<f64> {
    if !rate.is_finite() || rate <= 0.0 {
        None
    } else if invert {
        Some(1.0 / rate)
    } else {
        Some(rate)
    }
}

// Send one price on the CAN bus: 8 bytes (f64, little-endian)
fn send_price(sock: &CanSocket, id: u16, price: f64) -> Result<(), Box<dyn std::error::Error>> {
    let data = price.to_le_bytes();
    let can_id = StandardId::new(id).ok_or("invalid CAN ID")?;
    let frame = CanFrame::new(can_id, &data).ok_or("could not build CAN frame")?;
    sock.write_frame(&frame)?;
    Ok(())
}

// WORKER 2: heartbeat every second: [counter, crypto OK, markets OK]
fn heartbeat_loop(sock: CanSocket, crypto_ok: Arc<Mutex<u8>>, markets_ok: Arc<Mutex<u8>>) {
    let mut counter: u8 = 0;
    loop {
        let c = *crypto_ok.lock().unwrap();
        let m = *markets_ok.lock().unwrap();
        let data = [counter, c, m];
        if let Some(id) = StandardId::new(HEARTBEAT_ID) {
            if let Some(frame) = CanFrame::new(id, &data) {
                if let Err(e) = sock.write_frame(&frame) {
                    eprintln!("heartbeat CAN error: {}", e);
                }
            }
        }
        counter = counter.wrapping_add(1);
        sleep(Duration::from_secs(1));
    }
}

// One markets round: 2 downloads -> 10 prices -> CAN. Returns how many were sent OK.
fn markets_round(sock: &CanSocket) -> u8 {
    let rates = fetch_json(COINBASE_RATES_URL);
    let metals = fetch_json(XAUS_SPOT_URL);
    if let Err(e) = &rates {
        println!("markets: Coinbase error: {}", e);
    }
    if let Err(e) = &metals {
        println!("markets: xaus.com error: {}", e);
    }

    let mut ok: u8 = 0;
    for (i, (name, src)) in MARKETS.iter().enumerate() {
        // Pick the price from the right answer; None if anything is missing
        let price: Option<f64> = match src {
            Src::CoinbaseInvert(code) => rates
                .as_ref()
                .ok()
                .and_then(|j| coinbase_rate(j, code))
                .and_then(|r| rate_to_quote(r, true)),
            Src::CoinbaseDirect(code) => rates
                .as_ref()
                .ok()
                .and_then(|j| coinbase_rate(j, code))
                .and_then(|r| rate_to_quote(r, false)),
            Src::XausGold => metals.as_ref().ok().and_then(|j| j["spot_usd_oz"].as_f64()),
            Src::XausSilver => metals.as_ref().ok().and_then(|j| j["silver_usd_oz"].as_f64()),
        };

        let id = MARKETS_BASE_ID + i as u16;
        match price {
            Some(p) => match send_price(sock, id, p) {
                Ok(()) => {
                    ok += 1;
                    println!("sent  0x{:03X}  {:<8} = {:.5}", id, name, p);
                }
                Err(e) => println!("markets: CAN error for {}: {}", name, e),
            },
            None => println!("markets: no price for {}", name),
        }
    }
    ok
}

// WORKER 3: markets every 20 seconds, forever
fn markets_loop(sock: CanSocket, markets_ok: Arc<Mutex<u8>>) {
    loop {
        let ok = markets_round(&sock);
        *markets_ok.lock().unwrap() = ok;
        println!("--- markets: {}/10 OK", ok);
        sleep(Duration::from_secs(MARKETS_INTERVAL_S));
    }
}

fn main() {
    // One CAN connection per worker
    let sock = CanSocket::open("vcan0").expect("could not open vcan0 (does it exist and is it up?)");
    let hb_sock = CanSocket::open("vcan0").expect("could not open vcan0 for the heartbeat");
    let mk_sock = CanSocket::open("vcan0").expect("could not open vcan0 for the markets");

    // Shared OK counts (0 until the first round of each group is done)
    let crypto_ok = Arc::new(Mutex::new(0u8));
    let markets_ok = Arc::new(Mutex::new(0u8));

    // Start worker 2 (heartbeat)
    let crypto_for_hb = Arc::clone(&crypto_ok);
    let markets_for_hb = Arc::clone(&markets_ok);
    thread::spawn(move || heartbeat_loop(hb_sock, crypto_for_hb, markets_for_hb));

    // Start worker 3 (markets)
    let markets_for_mk = Arc::clone(&markets_ok);
    thread::spawn(move || markets_loop(mk_sock, markets_for_mk));

    println!("price-fetcher started: crypto every 5 s, markets every 20 s, heartbeat 0x0F0 every 1 s (Ctrl+C to stop)");

    // WORKER 1 (this main thread): crypto, as before
    loop {
        let mut ok_count: u8 = 0;
        for (index, pair) in PAIRS.iter().enumerate() {
            let id = CRYPTO_BASE_ID + index as u16;
            let coin = pair.split('-').next().unwrap_or(pair);
            match fetch_price(pair) {
                Ok(price) => match send_price(&sock, id, price) {
                    Ok(()) => {
                        ok_count += 1;
                        println!("sent  0x{:03X}  {:<8} = {:.4}", id, coin, price);
                    }
                    Err(e) => println!("CAN error for {}: {}", coin, e),
                },
                Err(e) => println!("fetch error for {}: {}", coin, e),
            }
        }
        *crypto_ok.lock().unwrap() = ok_count;
        println!("--- crypto: {}/6 OK", ok_count);
        sleep(Duration::from_secs(5));
    }
}
