// socketcan: CanSocket = bus connection, Socket = gives read_frame(),
// Frame = gives raw_id(), EmbeddedFrame = gives data()
use socketcan::{CanSocket, EmbeddedFrame, Frame, Socket};

// Same order as the fetcher: position 0 = ID 0x100 = BTC, ... position 5 = ID 0x105 = DOGE
const NAMES: [&str; 6] = ["BTC", "ETH", "SOL", "XRP", "ADA", "DOGE"];
const BASE_ID: u32 = 0x100;

fn main() {
    // Open the CAN bus (stop with a clear message if it doesn't exist)
    let sock = CanSocket::open("vcan0").expect("could not open vcan0 (does it exist and is it up?)");

    // Latest price of each coin. None = nothing received yet.
    let mut prices: [Option<f64>; 6] = [None; 6];
    // The price BEFORE the last change, to know if it went up or down
    let mut previous: [Option<f64>; 6] = [None; 6];
    // How many price frames we've received in total
    let mut count: u64 = 0;

    println!("price-dashboard: waiting for prices on vcan0... (Ctrl+C to stop)");

    loop {
        // 1. Wait for the next CAN frame on the bus
        let frame = match sock.read_frame() {
            Ok(f) => f,
            Err(e) => {
                eprintln!("read error: {}", e);
                continue; // skip and wait for the next one
            }
        };

        // 2. Keep only our price IDs (0x100 to 0x105)
        let id = frame.raw_id();
        if id < BASE_ID || id > BASE_ID + 5 {
            continue;
        }

        // 3. A price must be exactly 8 bytes
        let data = frame.data();
        if data.len() != 8 {
            continue;
        }

        // 4. Turn the 8 bytes back into a decimal number (opposite of to_le_bytes)
        let bytes: [u8; 8] = data.try_into().unwrap();
        let price = f64::from_le_bytes(bytes);

        // Which coin? ID 0x102 → position 2 → SOL
        let index = (id - BASE_ID) as usize;

        // If the price changed, remember the old one (for the arrow)
        if let Some(old) = prices[index] {
            if old != price {
                previous[index] = Some(old);
            }
        }
        prices[index] = Some(price);
        count += 1;

        // 5. Redraw the whole table
        print!("\x1B[2J\x1B[H"); // clear the screen, go to the top
        println!("==== PRICE DASHBOARD (from CAN bus vcan0) ====");
        println!("  ID     COIN            PRICE");
        for i in 0..6 {
            let can_id = BASE_ID + i as u32;
            match prices[i] {
                Some(p) => {
                    // ▲ if it went up since the last change, ▼ if it went down
                    let arrow = match previous[i] {
                        Some(old) if p > old => "▲",
                        Some(old) if p < old => "▼",
                        _ => " ",
                    };
                    println!("  0x{:03X}  {:<5} {:>14.4} USD {}", can_id, NAMES[i], p, arrow);
                }
                None => println!("  0x{:03X}  {:<5}     (waiting...)", can_id, NAMES[i]),
            }
        }
        println!("----------------------------------------------");
        println!("  price frames received: {}", count);
    }
}
