use std::sync::{Arc, Mutex};                         // shared state + lock
use std::thread;                                     // second worker
use std::time::{Instant, SystemTime, UNIX_EPOCH};    // Instant = stopwatch, SystemTime = clock

use serde_json::json;
use socketcan::{CanSocket, EmbeddedFrame, Frame, Socket};
use tiny_http::{Header, Response, Server, StatusCode};

// Prices: ID 0x100 + position = coin (same protocol as the fetcher)
const NAMES: [&str; 6] = ["BTC", "ETH", "SOL", "XRP", "ADA", "DOGE"];
const BASE_ID: u32 = 0x100;
// Gateway heartbeat: [alive counter, coins OK], sent every second by the fetcher
const HEARTBEAT_ID: u32 = 0x0F0;
// No heartbeat for more than 3 seconds = gateway offline
const HEARTBEAT_TIMEOUT_S: f64 = 3.0;
// Keep the last 200 prices per coin
const MAX_POINTS: usize = 200;

// The web page, built into the program at compile time
const PAGE: &str = include_str!("index.html");

// Everything the two workers share (protected by a lock)
struct State {
    history: Vec<Vec<(u64, f64)>>,   // for each coin: (time in seconds, price)
    last_hb: Option<Instant>,        // when the last heartbeat arrived (None = never)
    hb_counter: u8,                  // last heartbeat counter byte
    hb_coins_ok: u8,                 // last heartbeat "coins OK" byte
}

// Current time in seconds since 1970
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// WORKER 1: listen to the CAN bus forever
fn can_listener(sock: CanSocket, state: Arc<Mutex<State>>) {
    loop {
        let frame = match sock.read_frame() {
            Ok(f) => f,
            Err(e) => {
                eprintln!("CAN read error: {}", e);
                continue;
            }
        };
        let id = frame.raw_id();
        let data = frame.data();

        // Heartbeat from the gateway: remember WHEN it arrived and its 2 bytes
        if id == HEARTBEAT_ID {
            if data.len() >= 2 {
                let mut st = state.lock().unwrap();
                st.last_hb = Some(Instant::now());
                st.hb_counter = data[0];
                st.hb_coins_ok = data[1];
            }
            continue;
        }

        // Prices: only IDs 0x100..0x105 with exactly 8 bytes
        if id < BASE_ID || id > BASE_ID + 5 {
            continue;
        }
        if data.len() != 8 {
            continue;
        }
        let bytes: [u8; 8] = data.try_into().unwrap();
        let price = f64::from_le_bytes(bytes);
        let index = (id - BASE_ID) as usize;

        // Lock, add the point, drop the oldest if too many (unlocked at the end of the loop)
        let mut st = state.lock().unwrap();
        st.history[index].push((now_secs(), price));
        if st.history[index].len() > MAX_POINTS {
            st.history[index].remove(0);
        }
    }
}

// Everything the web page needs, as JSON:
// {"coins":[...], "gateway":{"seen":true,"alive":true,"age_s":0.4,"counter":42,"coins_ok":6}}
fn data_json(state: &Arc<Mutex<State>>) -> String {
    let st = state.lock().unwrap();

    let coins: Vec<serde_json::Value> = (0..6)
        .map(|i| {
            let points: Vec<serde_json::Value> =
                st.history[i].iter().map(|(t, p)| json!([t, p])).collect();
            json!({ "name": NAMES[i], "points": points })
        })
        .collect();

    // Seconds since the last heartbeat (None if none received yet -> null in JSON)
    let age: Option<f64> = st.last_hb.map(|t| t.elapsed().as_secs_f64());
    // Alive = a heartbeat arrived within the last 3 seconds
    let alive = matches!(age, Some(a) if a <= HEARTBEAT_TIMEOUT_S);

    json!({
        "coins": coins,
        "gateway": {
            "seen": st.last_hb.is_some(),
            "alive": alive,
            "age_s": age,
            "counter": st.hb_counter,
            "coins_ok": st.hb_coins_ok
        }
    })
    .to_string()
}

fn main() {
    // Open the CAN bus first: a missing vcan0 gives a clear error right away
    let sock = CanSocket::open("vcan0")
        .expect("could not open vcan0 (does it exist and is it up?)");

    // The shared state: empty history, no heartbeat yet
    let state = Arc::new(Mutex::new(State {
        history: vec![Vec::new(); 6],
        last_hb: None,
        hb_counter: 0,
        hb_coins_ok: 0,
    }));

    // Start worker 1 (CAN) with its own handle to the same state
    let state_for_can = Arc::clone(&state);
    thread::spawn(move || can_listener(sock, state_for_can));

    // WORKER 2 (this main thread): the web server on port 8080
    let server = Server::http("0.0.0.0:8080")
        .expect("could not start the web server on port 8080");
    println!("price-web started: open http://localhost:8080 in your browser (Ctrl+C to stop)");

    for request in server.incoming_requests() {
        let url = request.url().to_string();
        let response = match url.as_str() {
            "/" => Response::from_string(PAGE).with_header(
                Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap(),
            ),
            "/data" => Response::from_string(data_json(&state)).with_header(
                Header::from_bytes("Content-Type", "application/json").unwrap(),
            ),
            _ => Response::from_string("not found").with_status_code(StatusCode(404)),
        };
        let _ = request.respond(response);
    }
}
