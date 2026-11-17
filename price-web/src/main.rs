use std::io::Cursor;                                  // the type of a text response
use std::sync::{Arc, Mutex};                          // shared state + lock
use std::thread;                                      // second worker
use std::time::{Instant, SystemTime, UNIX_EPOCH};     // Instant = stopwatch, SystemTime = clock

use serde_json::json;
mod bus;                               // the bus: CAN on Linux, UDP elsewhere
use bus::Bus;
use tiny_http::{Header, Response, Server, StatusCode};

// ---------------- CAN ID map (same as price-fetcher) ----------------
const HEARTBEAT_ID: u32 = 0x0F0;     // [counter, crypto OK, markets OK]
const CRYPTO_BASE_ID: u32 = 0x100;   // 0x100..0x105
const MARKETS_BASE_ID: u32 = 0x200;  // 0x200..0x209

const CRYPTO_NAMES: [&str; 6] = ["BTC", "ETH", "SOL", "XRP", "ADA", "DOGE"];
const MARKET_NAMES: [&str; 10] = [
    "EUR/USD", "GBP/USD", "AUD/USD", "NZD/USD", "USD/JPY",
    "USD/CHF", "USD/CAD", "XAU/USD", "XAG/USD", "BTC/USD",
];
const N_MARKETS: usize = 10;

const HEARTBEAT_TIMEOUT_S: f64 = 3.0;  // no heartbeat for > 3 s = gateway offline
const MAX_POINTS: usize = 200;         // crypto: last 200 prices per coin
const MAX_CANDLES: usize = 1440;       // markets: 1440 one-minute candles = 24 hours

// The web page, built into the program at compile time
const PAGE: &str = include_str!("index.html");

// One 1-minute candle: start time of the minute, Open, High, Low, Close
#[derive(Clone, Copy, Debug, PartialEq)]
struct Candle {
    t: u64,
    o: f64,
    h: f64,
    l: f64,
    c: f64,
}

// Everything the two workers share (protected by a lock)
struct State {
    history: Vec<Vec<(u64, f64)>>,  // crypto: for each coin, (time, price)
    markets: Vec<Vec<Candle>>,      // markets: for each instrument, its candles
    last_hb: Option<Instant>,       // when the last heartbeat arrived (None = never)
    hb_counter: u8,
    hb_crypto_ok: u8,
    hb_markets_ok: u8,
    transport: String,              // "CAN bus vcan0" or "UDP 127.0.0.1:29540"
}

// Current time in seconds since 1970
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// 0x200..0x209 -> instrument 0..9, anything else -> None
fn market_index(id: u32) -> Option<usize> {
    if id >= MARKETS_BASE_ID && id < MARKETS_BASE_ID + N_MARKETS as u32 {
        Some((id - MARKETS_BASE_ID) as usize)
    } else {
        None
    }
}

// Add one price to an instrument's candles:
// same minute -> update High/Low/Close of the last candle, new minute -> start a new candle
fn update_candles(candles: &mut Vec<Candle>, t: u64, price: f64) {
    let minute = t - t % 60;
    match candles.last_mut() {
        Some(last) if last.t == minute => {
            if price > last.h {
                last.h = price;
            }
            if price < last.l {
                last.l = price;
            }
            last.c = price;
        }
        _ => {
            candles.push(Candle { t: minute, o: price, h: price, l: price, c: price });
            if candles.len() > MAX_CANDLES {
                candles.remove(0);
            }
        }
    }
}

// "/candles?i=3" -> Some(3); anything invalid -> None
fn candles_index(url: &str) -> Option<usize> {
    let n: usize = url.strip_prefix("/candles?i=")?.parse().ok()?;
    if n < N_MARKETS { Some(n) } else { None }
}

// WORKER 1: listen to the bus forever
fn bus_listener(sock: Bus, state: Arc<Mutex<State>>) {
    loop {
        let (id, data) = match sock.recv() {
            Ok(message) => message,
            Err(e) => {
                eprintln!("bus read error: {}", e);
                continue;
            }
        };
        let data = &data[..];

        // Heartbeat: [counter, crypto OK, markets OK (3rd byte only on newer fetchers)]
        if id == HEARTBEAT_ID {
            if data.len() >= 2 {
                let mut st = state.lock().unwrap();
                st.last_hb = Some(Instant::now());
                st.hb_counter = data[0];
                st.hb_crypto_ok = data[1];
                st.hb_markets_ok = if data.len() >= 3 { data[2] } else { 0 };
            }
            continue;
        }

        // Every price is 8 bytes (f64, little-endian)
        if data.len() != 8 {
            continue;
        }
        let bytes: [u8; 8] = data.try_into().unwrap();
        let price = f64::from_le_bytes(bytes);

        // Crypto: 0x100..0x105 -> line chart history
        if id >= CRYPTO_BASE_ID && id <= CRYPTO_BASE_ID + 5 {
            let index = (id - CRYPTO_BASE_ID) as usize;
            let mut st = state.lock().unwrap();
            st.history[index].push((now_secs(), price));
            if st.history[index].len() > MAX_POINTS {
                st.history[index].remove(0);
            }
        // Markets: 0x200..0x209 -> candles
        } else if let Some(index) = market_index(id) {
            let mut st = state.lock().unwrap();
            update_candles(&mut st.markets[index], now_secs(), price);
        }
    }
}

// Gateway status, shared by /data and /markets
fn gateway_json(st: &State) -> serde_json::Value {
    let age: Option<f64> = st.last_hb.map(|t| t.elapsed().as_secs_f64());
    let alive = matches!(age, Some(a) if a <= HEARTBEAT_TIMEOUT_S);
    json!({
        "seen": st.last_hb.is_some(),
        "alive": alive,
        "age_s": age,
        "counter": st.hb_counter,
        "coins_ok": st.hb_crypto_ok,
        "markets_ok": st.hb_markets_ok,
        "transport": st.transport
    })
}

// /data : crypto history + gateway (same as before, plus markets_ok)
fn data_json(state: &Arc<Mutex<State>>) -> String {
    let st = state.lock().unwrap();
    let coins: Vec<serde_json::Value> = (0..6)
        .map(|i| {
            let points: Vec<serde_json::Value> =
                st.history[i].iter().map(|(t, p)| json!([t, p])).collect();
            json!({ "name": CRYPTO_NAMES[i], "points": points })
        })
        .collect();
    json!({ "coins": coins, "gateway": gateway_json(&st) }).to_string()
}

// /markets : the 10 instruments with their last price and first price (for % change)
fn markets_json(state: &Arc<Mutex<State>>) -> String {
    let st = state.lock().unwrap();
    let list: Vec<serde_json::Value> = (0..N_MARKETS)
        .map(|i| {
            let candles = &st.markets[i];
            json!({
                "name": MARKET_NAMES[i],
                "id": MARKETS_BASE_ID + i as u32,
                "last": candles.last().map(|k| k.c),
                "first": candles.first().map(|k| k.o),
                "candles": candles.len()
            })
        })
        .collect();
    json!({ "markets": list, "gateway": gateway_json(&st) }).to_string()
}

// /candles?i=N : all candles of one instrument as [time, open, high, low, close]
fn candles_json(state: &Arc<Mutex<State>>, i: usize) -> String {
    let st = state.lock().unwrap();
    let candles: Vec<serde_json::Value> = st.markets[i]
        .iter()
        .map(|k| json!([k.t, k.o, k.h, k.l, k.c]))
        .collect();
    json!({ "name": MARKET_NAMES[i], "candles": candles }).to_string()
}

// A JSON answer for the browser
fn json_response(body: String) -> Response<Cursor<Vec<u8>>> {
    Response::from_string(body)
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
}

fn main() {
    // On Linux this listens on CAN (vcan0); with --udp, or on a machine without
    // CAN, it listens for UDP messages on 127.0.0.1 instead.
    let sock = Bus::open_receiver(bus::udp_requested())
        .expect("could not open the bus (on Linux: does vcan0 exist and is it up?)");
    println!("price-web: listening on {}", sock.name());

    let state = Arc::new(Mutex::new(State {
        history: vec![Vec::new(); 6],
        markets: vec![Vec::new(); N_MARKETS],
        last_hb: None,
        hb_counter: 0,
        hb_crypto_ok: 0,
        hb_markets_ok: 0,
        transport: sock.name(),     // so the page can say what it is really using
    }));

    let state_for_bus = Arc::clone(&state);
    thread::spawn(move || bus_listener(sock, state_for_bus));

    let server = Server::http("0.0.0.0:8080")
        .expect("could not start the web server on port 8080");
    println!("price-web started: open http://localhost:8080 in your browser (Ctrl+C to stop)");

    for request in server.incoming_requests() {
        let url = request.url().to_string();
        let response = if url == "/" {
            Response::from_string(PAGE)
                .with_header(Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap())
        } else if url == "/data" {
            json_response(data_json(&state))
        } else if url == "/markets" {
            json_response(markets_json(&state))
        } else if let Some(i) = candles_index(&url) {
            json_response(candles_json(&state, i))
        } else {
            Response::from_string("not found").with_status_code(StatusCode(404))
        };
        let _ = request.respond(response);
    }
}

// ---------------- Unit tests (run with: cargo test) ----------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn market_ids_map_to_instruments() {
        assert_eq!(market_index(0x200), Some(0));
        assert_eq!(market_index(0x209), Some(9));
        assert_eq!(market_index(0x20A), None); // just past the last instrument
        assert_eq!(market_index(0x105), None); // a crypto ID
        assert_eq!(market_index(0x0F0), None); // the heartbeat
    }

    #[test]
    fn candles_url_is_parsed_safely() {
        assert_eq!(candles_index("/candles?i=0"), Some(0));
        assert_eq!(candles_index("/candles?i=9"), Some(9));
        assert_eq!(candles_index("/candles?i=10"), None);
        assert_eq!(candles_index("/candles?i=x"), None);
        assert_eq!(candles_index("/data"), None);
    }

    #[test]
    fn prices_in_the_same_minute_build_one_candle() {
        let mut v = Vec::new();
        update_candles(&mut v, 120, 10.0); // 02:00 open 10
        update_candles(&mut v, 130, 12.0); // new high
        update_candles(&mut v, 150, 9.0);  // new low
        update_candles(&mut v, 179, 11.0); // close
        assert_eq!(v, vec![Candle { t: 120, o: 10.0, h: 12.0, l: 9.0, c: 11.0 }]);
        update_candles(&mut v, 180, 11.5); // next minute -> new candle
        assert_eq!(v.len(), 2);
        assert_eq!(v[1], Candle { t: 180, o: 11.5, h: 11.5, l: 11.5, c: 11.5 });
    }

    #[test]
    fn only_24_hours_of_candles_are_kept() {
        let mut v = Vec::new();
        for k in 0..(MAX_CANDLES as u64 + 5) {
            update_candles(&mut v, k * 60, k as f64);
        }
        assert_eq!(v.len(), MAX_CANDLES);
        assert_eq!(v[0].t, 5 * 60); // the 5 oldest were dropped
    }
}
