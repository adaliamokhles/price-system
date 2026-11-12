use std::sync::{Arc, Mutex};                 // Arc = shared ownership, Mutex = lock
use std::thread;                             // to start a second worker (thread)
use std::time::{SystemTime, UNIX_EPOCH};     // to timestamp each price

use serde_json::json;                        // json!(...) builds JSON easily
use socketcan::{CanSocket, EmbeddedFrame, Frame, Socket};
use tiny_http::{Header, Response, Server, StatusCode};

// Same protocol as the fetcher: ID 0x100 + position = coin
const NAMES: [&str; 6] = ["BTC", "ETH", "SOL", "XRP", "ADA", "DOGE"];
const BASE_ID: u32 = 0x100;
// Keep the last 200 prices per coin (at 1 every 5 s = about 16 minutes of chart)
const MAX_POINTS: usize = 200;

// The web page, read from src/index.html and built INTO the program at compile time
const PAGE: &str = include_str!("index.html");

// For each of the 6 coins: a list of (time in seconds, price)
type History = Vec<Vec<(u64, f64)>>;

// Current time in seconds since 1970 (the usual way computers count time)
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// THREAD 1: listen to the CAN bus forever and save every price
fn can_listener(sock: CanSocket, history: Arc<Mutex<History>>) {
    loop {
        // Wait for the next frame
        let frame = match sock.read_frame() {
            Ok(f) => f,
            Err(e) => {
                eprintln!("CAN read error: {}", e);
                continue;
            }
        };

        // Keep only our 6 price IDs, with exactly 8 data bytes
        let id = frame.raw_id();
        if id < BASE_ID || id > BASE_ID + 5 {
            continue;
        }
        let data = frame.data();
        if data.len() != 8 {
            continue;
        }

        // 8 bytes -> price (same rule as the fetcher and the dashboard)
        let bytes: [u8; 8] = data.try_into().unwrap();
        let price = f64::from_le_bytes(bytes);
        let index = (id - BASE_ID) as usize;

        // LOCK the shared history, add the point, drop the oldest if too many.
        // The lock is released automatically at the end of this block.
        let mut h = history.lock().unwrap();
        h[index].push((now_secs(), price));
        if h[index].len() > MAX_POINTS {
            h[index].remove(0);
        }
    }
}

// Turn the history into JSON for the web page:
// {"coins":[{"name":"BTC","points":[[time,price],...]}, ...]}
fn data_json(history: &Arc<Mutex<History>>) -> String {
    let h = history.lock().unwrap();
    let coins: Vec<serde_json::Value> = (0..6)
        .map(|i| {
            let points: Vec<serde_json::Value> =
                h[i].iter().map(|(t, p)| json!([t, p])).collect();
            json!({ "name": NAMES[i], "points": points })
        })
        .collect();
    json!({ "coins": coins }).to_string()
}

fn main() {
    // Open the CAN bus FIRST, so a missing vcan0 gives a clear error right away
    let sock = CanSocket::open("vcan0")
        .expect("could not open vcan0 (does it exist and is it up?)");

    // The shared history: 6 empty lists, wrapped in a lock (Mutex) + shared pointer (Arc)
    let history: Arc<Mutex<History>> = Arc::new(Mutex::new(vec![Vec::new(); 6]));

    // Give thread 1 its own handle to the SAME history, and start it.
    // "move" = the thread takes ownership of sock and history_for_can.
    let history_for_can = Arc::clone(&history);
    thread::spawn(move || can_listener(sock, history_for_can));

    // THREAD 2 (this main thread): the web server on port 8080.
    // 0.0.0.0 = accept connections from any network card, not only from this machine.
    let server = Server::http("0.0.0.0:8080")
        .expect("could not start the web server on port 8080");
    println!("price-web started: open http://localhost:8080 in your browser (Ctrl+C to stop)");

    // Answer each browser request, one after another, forever
    for request in server.incoming_requests() {
        let url = request.url().to_string();
        let response = match url.as_str() {
            // The web page
            "/" => Response::from_string(PAGE).with_header(
                Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap(),
            ),
            // The price history as JSON
            "/data" => Response::from_string(data_json(&history)).with_header(
                Header::from_bytes("Content-Type", "application/json").unwrap(),
            ),
            // Anything else: "404 not found"
            _ => Response::from_string("not found").with_status_code(StatusCode(404)),
        };
        // Send the answer back (ignore errors, e.g. if the browser closed the tab)
        let _ = request.respond(response);
    }
}
