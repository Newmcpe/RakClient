//! Minimal libpcap-format writer for capturing a session's RakNet UDP datagrams (`--pcap`).
//!
//! Each datagram is wrapped in a synthetic IPv4+UDP header (LINKTYPE_RAW) so the file opens directly in
//! Wireshark, with client↔server direction shown by src/dst. Outbound datagrams are byte-ciphered (the
//! client encrypts); inbound are plaintext (the server replies in the clear), so a server kick /
//! disconnect reads directly. The file is flushed per record so a crash still leaves a valid capture.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Synthetic local endpoint for the client side of each datagram — the real local addr is unknown
/// behind a proxy, and only the client↔server direction matters for analysis.
const CLIENT_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 13, 37, 1)), 1337);

/// Writes RakNet datagrams to a libpcap file. Cheap to hold; `record` is `&self` (internal `Mutex`).
pub struct PcapWriter {
    file: Mutex<BufWriter<File>>,
    server: SocketAddr,
}

impl PcapWriter {
    /// Create `path` (truncating any previous capture) and write the pcap global header.
    pub fn create(path: &Path, server: SocketAddr) -> std::io::Result<Self> {
        let mut w = BufWriter::new(File::create(path)?);
        w.write_all(&0xa1b2_c3d4u32.to_le_bytes())?; // magic
        w.write_all(&2u16.to_le_bytes())?; // version major
        w.write_all(&4u16.to_le_bytes())?; // version minor
        w.write_all(&0i32.to_le_bytes())?; // thiszone (GMT offset)
        w.write_all(&0u32.to_le_bytes())?; // sigfigs
        w.write_all(&65_535u32.to_le_bytes())?; // snaplen
        w.write_all(&101u32.to_le_bytes())?; // LINKTYPE_RAW: each record is a raw IPv4 packet
        w.flush()?;
        Ok(Self {
            file: Mutex::new(w),
            server,
        })
    }

    /// Record one datagram. `outbound` = client→server.
    pub fn record(&self, outbound: bool, payload: &[u8]) {
        let (src, dst) = if outbound {
            (CLIENT_ADDR, self.server)
        } else {
            (self.server, CLIENT_ADDR)
        };
        let src_ip = match src.ip() {
            IpAddr::V4(ip) => ip,
            IpAddr::V6(_) => return, // IPv4 only (Arizona servers are v4)
        };
        let dst_ip = match dst.ip() {
            IpAddr::V4(ip) => ip,
            IpAddr::V6(_) => return,
        };

        let payload = &payload[..payload.len().min(65_507)];
        let udp_len = 8 + payload.len();
        let ip_total = 20 + udp_len;

        let mut pkt = Vec::with_capacity(ip_total);
        // IPv4 header (network order / big-endian). Checksums left 0 (unverified) — Wireshark reads it.
        pkt.push(0x45); // version 4, IHL 5 (20-byte header)
        pkt.push(0x00); // DSCP/ECN
        pkt.extend_from_slice(&(ip_total as u16).to_be_bytes());
        pkt.extend_from_slice(&0u16.to_be_bytes()); // identification
        pkt.extend_from_slice(&0x4000u16.to_be_bytes()); // flags: Don't Fragment
        pkt.push(64); // TTL
        pkt.push(17); // protocol: UDP
        pkt.extend_from_slice(&0u16.to_be_bytes()); // header checksum (0)
        pkt.extend_from_slice(&src_ip.octets());
        pkt.extend_from_slice(&dst_ip.octets());
        // UDP header
        pkt.extend_from_slice(&src.port().to_be_bytes());
        pkt.extend_from_slice(&dst.port().to_be_bytes());
        pkt.extend_from_slice(&(udp_len as u16).to_be_bytes());
        pkt.extend_from_slice(&0u16.to_be_bytes()); // checksum (0 = unused for IPv4/UDP)
        pkt.extend_from_slice(payload);

        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        if let Ok(mut w) = self.file.lock() {
            let _ = w.write_all(&(ts.as_secs() as u32).to_le_bytes());
            let _ = w.write_all(&ts.subsec_micros().to_le_bytes());
            let _ = w.write_all(&(pkt.len() as u32).to_le_bytes()); // incl_len
            let _ = w.write_all(&(pkt.len() as u32).to_le_bytes()); // orig_len
            let _ = w.write_all(&pkt);
            let _ = w.flush(); // durable per-record: a crash still leaves a valid pcap
        }
    }
}
