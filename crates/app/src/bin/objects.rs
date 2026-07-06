//! `objects` — extract server-streamed CreateObject (RPC 44) placements from a `--pcap` capture.
//!
//! Arizona streams its custom map (the parts not in the vanilla IPLs — job zones, decorations, the
//! sawmill) to the client as standard SA-MP `CreateObject` RPCs. Each carries a model id + world
//! position + euler rotation. This mines them from an existing capture so the viewer can overlay them
//! on the base collision and we can SEE the custom world the bot actually lives in.
//!
//! RPC 44 args (byte-aligned inside the de-framed RPC payload):
//!   u16 objectId, i32 modelId, f32×3 position, f32×3 rotation(euler°), f32 drawDistance, …
//!
//! Usage: `cargo run -p app --bin objects -- <capture.pcap> [out.csv]`

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::net::Ipv4Addr;

const CLIENT_IP: Ipv4Addr = Ipv4Addr::new(10, 13, 37, 1);
const RPC_CREATE_OBJECT: u8 = 44;

fn le_f32(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = match args.next() {
        Some(p) => p,
        None => {
            eprintln!("usage: objects <capture.pcap> [out.csv]");
            std::process::exit(2);
        }
    };
    let out_path = args.next().unwrap_or_else(|| format!("{path}.objects.csv"));
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            std::process::exit(1);
        }
    };
    if bytes.len() < 24 || bytes[0..4] != [0xd4, 0xc3, 0xb2, 0xa1] {
        eprintln!("not a little-endian libpcap file");
        std::process::exit(1);
    }

    // Keyed by objectId so a re-created object keeps only its latest placement.
    let mut objects: HashMap<u16, (i32, [f32; 3], [f32; 3])> = HashMap::new();
    // In-flight split reassembly: split id → fragment index → fragment bytes.
    let mut splits: HashMap<u16, BTreeMap<u32, Vec<u8>>> = HashMap::new();
    let mut off = 24;
    while off + 16 <= bytes.len() {
        let incl = u32::from_le_bytes(bytes[off + 8..off + 12].try_into().unwrap()) as usize;
        off += 16;
        if off + incl > bytes.len() {
            break;
        }
        let pkt = &bytes[off..off + incl];
        off += incl;

        // IPv4 + UDP, inbound only (server → client; inbound is plaintext).
        if pkt.len() < 28 || (pkt[0] >> 4) != 4 {
            continue;
        }
        let ihl = (pkt[0] & 0x0f) as usize * 4;
        if pkt.len() < ihl + 8 {
            continue;
        }
        let src_ip = Ipv4Addr::new(pkt[12], pkt[13], pkt[14], pkt[15]);
        if src_ip == CLIENT_IP {
            continue; // outbound
        }
        let payload = &pkt[ihl + 8..];

        let (_acks, msgs) = raknet::dissect_datagram(payload);
        for m in &msgs {
            // Large RPCs arrive as split fragments (a building's CreateObject carries per-material
            // texture data and easily exceeds one datagram). Buffer by split id and parse only the
            // reassembled whole.
            let whole: Vec<u8>;
            let msg_payload: &[u8] = match m.split {
                None => &m.payload,
                Some(s) => {
                    let buf = splits.entry(s.id).or_default();
                    buf.insert(s.index, m.payload.clone());
                    if buf.len() as u32 != s.count {
                        continue; // still missing fragments
                    }
                    let buf = splits.remove(&s.id).unwrap();
                    whole = buf.into_values().flatten().collect();
                    &whole
                }
            };
            let Some((rpc_id, body)) = raknet::wire::parse_rpc(msg_payload) else {
                continue;
            };
            if rpc_id != RPC_CREATE_OBJECT || body.len() < 30 {
                continue;
            }
            let object_id = u16::from_le_bytes([body[0], body[1]]);
            let model_id = i32::from_le_bytes([body[2], body[3], body[4], body[5]]);
            let pos = [le_f32(&body, 6), le_f32(&body, 10), le_f32(&body, 14)];
            let rot = [le_f32(&body, 18), le_f32(&body, 22), le_f32(&body, 26)];
            objects.insert(object_id, (model_id, pos, rot));
        }
    }

    // Write CSV + summary.
    let mut f = match std::fs::File::create(&out_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("cannot write {out_path}: {e}");
            std::process::exit(1);
        }
    };
    let _ = writeln!(f, "model_id,x,y,z,rx,ry,rz");
    let mut models: HashMap<i32, u32> = HashMap::new();
    for (model, p, r) in objects.values() {
        *models.entry(*model).or_default() += 1;
        let _ = writeln!(
            f,
            "{model},{:.3},{:.3},{:.3},{:.2},{:.2},{:.2}",
            p[0], p[1], p[2], r[0], r[1], r[2]
        );
    }
    let mut top: Vec<_> = models.iter().collect();
    top.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
    eprintln!(
        "extracted {} CreateObject placements ({} distinct models) → {out_path}",
        objects.len(),
        models.len()
    );
    eprintln!("top models:");
    for (m, c) in top.iter().take(15) {
        eprintln!("  {c:>4}  model {m}");
    }
}
