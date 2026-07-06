//! `dissect` — offline decoder for the `--pcap` captures written by `rakclient --pcap`.
//!
//! Reads the libpcap file, and for each RakNet datagram: determines direction (client↔server from the
//! synthetic IPs), decrypts outbound datagrams (the client byte-ciphers them; the server replies in the
//! clear), then parses the RakNet 3.x reliability framing into its internal messages and names the
//! leading message id. Prints one block per datagram so a disconnect/kick is easy to trace.
//!
//! Usage: `cargo run -p app --bin dissect -- <capture.pcap>`

use std::net::Ipv4Addr;

/// The synthetic client IP the capture writer stamps on outbound datagrams (see `raknet::pcap`).
const CLIENT_IP: Ipv4Addr = Ipv4Addr::new(10, 13, 37, 1);

fn main() {
    let path = match std::env::args().nth(1) {
        Some(p) => p,
        None => {
            eprintln!("usage: dissect <capture.pcap>");
            std::process::exit(2);
        }
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            std::process::exit(1);
        }
    };

    if bytes.len() < 24 || bytes[0..4] != [0xd4, 0xc3, 0xb2, 0xa1] {
        eprintln!("not a little-endian libpcap file (bad magic)");
        std::process::exit(1);
    }

    let mut off = 24; // skip global header
    let mut first_ts: Option<f64> = None;
    let mut n = 0u64;
    while off + 16 <= bytes.len() {
        let ts_sec = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        let ts_usec = u32::from_le_bytes(bytes[off + 4..off + 8].try_into().unwrap());
        let incl = u32::from_le_bytes(bytes[off + 8..off + 12].try_into().unwrap()) as usize;
        off += 16;
        if off + incl > bytes.len() {
            break;
        }
        let pkt = &bytes[off..off + incl];
        off += incl;
        n += 1;

        let ts = ts_sec as f64 + ts_usec as f64 / 1e6;
        let rel = *first_ts.get_or_insert(ts);
        dissect_record(n, ts - rel, pkt);
    }
    println!("\n{n} datagram(s).");
}

/// Parse one LINKTYPE_RAW record: IPv4 + UDP + RakNet payload, then dissect the payload.
fn dissect_record(n: u64, t: f64, pkt: &[u8]) {
    if pkt.len() < 28 || (pkt[0] >> 4) != 4 {
        return; // not IPv4
    }
    let ihl = (pkt[0] & 0x0f) as usize * 4;
    if pkt.len() < ihl + 8 {
        return;
    }
    let src_ip = Ipv4Addr::new(pkt[12], pkt[13], pkt[14], pkt[15]);
    let udp = &pkt[ihl..];
    let src_port = u16::from_be_bytes([udp[0], udp[1]]);
    let dst_port = u16::from_be_bytes([udp[2], udp[3]]);
    let payload = &udp[8..];

    let outbound = src_ip == CLIENT_IP;
    let (dir, server_port) = if outbound {
        ("OUT", dst_port)
    } else {
        ("IN ", src_port)
    };

    // Outbound connected datagrams are byte-ciphered; try to decrypt. Offline outbound (handshake /
    // query ping) is plaintext, so a failed decrypt means "treat as raw". Inbound is always plaintext.
    let (plaintext, ciphered) = if outbound {
        match raknet::cipher::decrypt(payload, server_port) {
            Ok(p) => (p, true),
            Err(_) => (payload.to_vec(), false),
        }
    } else {
        (payload.to_vec(), false)
    };

    print!("[{t:8.3}] #{n:<5} {dir} {:4}B", payload.len());
    if ciphered {
        print!(" (deciphered {}B)", plaintext.len());
    }

    // Offline / query datagrams aren't reliability-framed: label by their leading id.
    if plaintext.starts_with(b"SAMP") {
        println!("  SAMP query ping/pong");
        return;
    }

    let (acks, msgs) = raknet::dissect_datagram(&plaintext);
    if msgs.is_empty() {
        // Not reliability-framed (or empty): show the raw leading id as an offline message.
        let id = plaintext.first().copied().unwrap_or(0);
        println!("  offline id={id} (0x{id:02X}) {}", name(id));
        return;
    }
    if !acks.is_empty() {
        print!("  acks={acks:?}");
    }
    println!();
    for m in &msgs {
        let id = m.payload.first().copied().unwrap_or(0);
        let split = match m.split {
            Some(s) => format!(" SPLIT {}/{} id={}", s.index + 1, s.count, s.id),
            None => String::new(),
        };
        // For an RPC the leading id is the RakNet RPC marker and byte 1 is the SA-MP RPC id.
        let extra = if id == RPC_MARKER {
            m.payload
                .get(1)
                .map(|r| format!(" rpc={r} (0x{r:02X})"))
                .unwrap_or_default()
        } else {
            String::new()
        };
        println!(
            "        {:>4?} #{:<5}{split} id={id:3} (0x{id:02X}) {}{extra}  [{}]",
            m.reliability,
            m.message_number,
            name(id),
            hex_head(&m.payload, 24),
        );
    }
}

/// SA-MP's RakNet RPC message marker (`ID_RPC`); byte 1 is then the SA-MP RPC id.
const RPC_MARKER: u8 = raknet::wire::ID_RPC;

fn name(id: u8) -> &'static str {
    match id {
        // RakNet system / connection messages (the ones that matter for disconnects)
        29 => "CONNECTION_ATTEMPT_FAILED",
        30 => "ALREADY_CONNECTED",
        31 => "NO_FREE_INCOMING_CONNECTIONS",
        32 => "DISCONNECTION_NOTIFICATION",
        33 => "CONNECTION_LOST",
        34 => "CONNECTION_REQUEST_ACCEPTED",
        36 => "RPC / CONNECTION_BANNED",
        37 => "INVALID_PASSWORD",
        // SA-MP application packets (message id == packet id)
        200 => "VehicleSync(200)",
        201 => "RconCommand(201)",
        203 => "AimSync(203)",
        205 => "BulletSync(205)",
        206 => "WeaponsUpdate(206)",
        207 => "OnFootSync(207)",
        211 => "PassengerSync(211)",
        212 => "SpectatorSync(212)",
        // Arizona custom channels
        220 => "Arizona-CEF(220)",
        221 => "Arizona-sync(221)",
        _ => "",
    }
}

/// First `n` bytes of `data` as space-separated hex.
fn hex_head(data: &[u8], n: usize) -> String {
    data.iter()
        .take(n)
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
