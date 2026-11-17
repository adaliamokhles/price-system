// The bus that the three programs talk over.
//
// On the device (and on any Linux machine) this is a real CAN bus: the kind of
// network used in cars, where every message carries an ID instead of an address.
//
// CAN lives inside the Linux kernel, so it does not exist on macOS or Windows.
// For those machines there is a second transport: ordinary UDP messages sent to
// 127.0.0.1. The programs stay separate processes talking to each other over a
// socket -- only the pipe between them changes.
//
// A UDP message is simply a CAN frame written out by hand:
//
//     [ID high byte][ID low byte][up to 8 data bytes]
//
// Which transport is used:
//   Linux            -> CAN, unless the program is started with --udp
//   anything else     -> UDP (the CAN code is not even compiled)

// The same file is used by all three programs, and each one uses only part of
// it (the fetcher sends, the others receive), so unused halves are expected.
#![allow(dead_code)]

use std::io;
use std::net::UdpSocket;

/// Where the UDP transport meets: the receiver listens here, senders send here.
pub const UDP_ADDR: &str = "127.0.0.1:29540";

/// The CAN interface used on Linux.
pub const CAN_IFACE: &str = "vcan0";

/// True on machines where CAN was compiled in.
pub const CAN_SUPPORTED: bool = cfg!(all(target_os = "linux", not(feature = "udp-only")));

// ---------------------------------------------------------------- CAN half
// This whole module exists only on Linux. On macOS it is removed before
// compilation, which is why the socketcan crate is never needed there.
#[cfg(all(target_os = "linux", not(feature = "udp-only")))]
mod can {
    use socketcan::{CanFrame, CanSocket, EmbeddedFrame, Frame, Socket, StandardId};
    use std::io;

    pub type Sock = CanSocket;

    pub fn open(iface: &str) -> io::Result<Sock> {
        CanSocket::open(iface).map_err(|e| io::Error::other(e.to_string()))
    }

    pub fn send(sock: &Sock, id: u16, data: &[u8]) -> io::Result<()> {
        let can_id = StandardId::new(id).ok_or_else(|| io::Error::other("invalid CAN ID"))?;
        let frame =
            CanFrame::new(can_id, data).ok_or_else(|| io::Error::other("could not build frame"))?;
        sock.write_frame(&frame)
            .map_err(|e| io::Error::other(e.to_string()))
    }

    pub fn recv(sock: &Sock) -> io::Result<(u32, Vec<u8>)> {
        let frame = sock
            .read_frame()
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok((frame.raw_id(), frame.data().to_vec()))
    }
}

// ---------------------------------------------------------------- the bus
/// One connection to the bus, whichever transport it turned out to be.
pub enum Bus {
    #[cfg(all(target_os = "linux", not(feature = "udp-only")))]
    Can(can::Sock),
    Udp(UdpSocket),
}

impl Bus {
    /// A connection for sending messages (used by price-fetcher).
    pub fn open_sender(prefer_udp: bool) -> io::Result<Bus> {
        let _ = prefer_udp; // only consulted on machines that have CAN
        #[cfg(all(target_os = "linux", not(feature = "udp-only")))]
        if !prefer_udp {
            return Ok(Bus::Can(can::open(CAN_IFACE)?));
        }
        let sock = UdpSocket::bind("127.0.0.1:0")?; // any free port
        sock.connect(UDP_ADDR)?; // always send to the receiver
        Ok(Bus::Udp(sock))
    }

    /// A connection for receiving messages (used by price-web and price-dashboard).
    pub fn open_receiver(prefer_udp: bool) -> io::Result<Bus> {
        let _ = prefer_udp; // only consulted on machines that have CAN
        #[cfg(all(target_os = "linux", not(feature = "udp-only")))]
        if !prefer_udp {
            return Ok(Bus::Can(can::open(CAN_IFACE)?));
        }
        Ok(Bus::Udp(UdpSocket::bind(UDP_ADDR)?))
    }

    /// Put one message on the bus: an ID plus up to 8 bytes.
    pub fn send(&self, id: u16, data: &[u8]) -> io::Result<()> {
        match self {
            #[cfg(all(target_os = "linux", not(feature = "udp-only")))]
            Bus::Can(sock) => can::send(sock, id, data),
            Bus::Udp(sock) => {
                sock.send(&pack(id, data))?;
                Ok(())
            }
        }
    }

    /// Wait for the next message. Returns its ID and its bytes.
    pub fn recv(&self) -> io::Result<(u32, Vec<u8>)> {
        match self {
            #[cfg(all(target_os = "linux", not(feature = "udp-only")))]
            Bus::Can(sock) => can::recv(sock),
            Bus::Udp(sock) => {
                let mut buf = [0u8; 64];
                let (n, _from) = sock.recv_from(&mut buf)?;
                unpack(&buf[..n])
            }
        }
    }

    /// For printing at start-up, so it is obvious which transport is in use.
    pub fn name(&self) -> String {
        match self {
            #[cfg(all(target_os = "linux", not(feature = "udp-only")))]
            Bus::Can(_) => format!("CAN bus {}", CAN_IFACE),
            Bus::Udp(_) => format!("UDP {}", UDP_ADDR),
        }
    }
}

/// Was the program started with --udp? On machines without CAN the answer is
/// always yes, whatever was typed.
pub fn udp_requested() -> bool {
    !CAN_SUPPORTED || std::env::args().any(|a| a == "--udp")
}

/// ID + data -> the bytes that travel in one UDP message.
fn pack(id: u16, data: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(2 + data.len());
    msg.push((id >> 8) as u8); // high byte of the ID
    msg.push(id as u8); // low byte of the ID
    msg.extend_from_slice(data);
    msg
}

/// The bytes of one UDP message -> ID + data.
fn unpack(msg: &[u8]) -> io::Result<(u32, Vec<u8>)> {
    if msg.len() < 2 {
        return Err(io::Error::other("message too short to hold an ID"));
    }
    let id = ((msg[0] as u32) << 8) | msg[1] as u32;
    Ok((id, msg[2..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_and_its_bytes_survive_the_trip() {
        let price: f64 = 64608.21;
        let msg = pack(0x205, &price.to_le_bytes());
        assert_eq!(msg.len(), 10); // 2 for the ID + 8 for the price
        let (id, data) = unpack(&msg).unwrap();
        assert_eq!(id, 0x205);
        assert_eq!(f64::from_le_bytes(data.try_into().unwrap()), price);
    }

    #[test]
    fn a_three_byte_heartbeat_survives_too() {
        let (id, data) = unpack(&pack(0x0F0, &[7, 6, 10])).unwrap();
        assert_eq!(id, 0x0F0);
        assert_eq!(data, vec![7, 6, 10]);
    }

    #[test]
    fn a_damaged_message_is_an_error_not_a_crash() {
        assert!(unpack(&[]).is_err());
        assert!(unpack(&[0x02]).is_err());
        // An ID with no data is odd but not damaged: it simply carries nothing
        assert_eq!(unpack(&[0x02, 0x05]).unwrap(), (0x205, Vec::new()));
    }

    #[test]
    fn two_programs_can_talk_over_udp() {
        // Exactly what happens on a machine without CAN: one process opens the
        // receiver, another opens a sender, and the messages arrive in order.
        let receiver = Bus::open_receiver(true).expect("could not open the receiver");
        let sender = Bus::open_sender(true).expect("could not open the sender");

        sender.send(0x100, &1234.5f64.to_le_bytes()).unwrap();
        sender.send(0x0F0, &[42, 6, 10]).unwrap();

        let (id, data) = receiver.recv().unwrap();
        assert_eq!(id, 0x100);
        assert_eq!(f64::from_le_bytes(data.try_into().unwrap()), 1234.5);

        let (id, data) = receiver.recv().unwrap();
        assert_eq!(id, 0x0F0);
        assert_eq!(data, vec![42, 6, 10]);
    }
}
