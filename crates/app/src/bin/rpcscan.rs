//! `rpcscan` — inbound RPC/packet census for a `--pcap` capture, plus a bit-level needle search.
//!
//! Counts every inbound (server→client, plaintext) RPC id and raw packet id in a capture, with total
//! and max body sizes — the quick way to see WHICH channel carries something. With needle ids given,
//! also reports which RPC/packet bodies contain each needle as a little-endian i32 at any byte offset
//! (bodies are byte-aligned after reassembly, so a plain scan is exact).
//!
//! Usage: `cargo run -p app --bin rpcscan -- <capture.pcap> [needle-i32]...`

use std::collections::{BTreeMap, HashMap};
use std::net::Ipv4Addr;

const CLIENT_IP: Ipv4Addr = Ipv4Addr::new(10, 13, 37, 1);

struct Stat {
    count: u32,
    total: u64,
    max: usize,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = match args.next() {
        Some(p) => p,
        None => {
            eprintln!("usage: rpcscan <capture.pcap> [needle-i32]...");
            std::process::exit(2);
        }
    };
    let needles: Vec<i32> = args.filter_map(|s| s.parse().ok()).collect();

    let mut rpc_stats: BTreeMap<u8, Stat> = BTreeMap::new();
    let mut pkt_stats: BTreeMap<u8, Stat> = BTreeMap::new();
    // needle → "kind id" → hit count (kind: "rpc N" / "pkt N")
    let mut needle_hits: HashMap<i32, BTreeMap<String, u32>> = HashMap::new();

    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("cannot read {path}: {e}");
        std::process::exit(1);
    });
    if bytes.len() < 24 || bytes[0..4] != [0xd4, 0xc3, 0xb2, 0xa1] {
        eprintln!("not a little-endian libpcap file");
        std::process::exit(1);
    }

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
        if pkt.len() < 28 || (pkt[0] >> 4) != 4 {
            continue;
        }
        let ihl = (pkt[0] & 0x0f) as usize * 4;
        if pkt.len() < ihl + 8 {
            continue;
        }
        let src_ip = Ipv4Addr::new(pkt[12], pkt[13], pkt[14], pkt[15]);
        if src_ip == CLIENT_IP {
            continue;
        }
        let payload = &pkt[ihl + 8..];

        let (_acks, msgs) = raknet::dissect_datagram(payload);
        for m in &msgs {
            let whole: Vec<u8>;
            let payload: &[u8] = match m.split {
                None => &m.payload,
                Some(s) => {
                    let buf = splits.entry(s.id).or_default();
                    buf.insert(s.index, m.payload.clone());
                    if buf.len() as u32 != s.count {
                        continue;
                    }
                    let buf = splits.remove(&s.id).unwrap();
                    whole = buf.into_values().flatten().collect();
                    &whole
                }
            };
            match raknet::wire::parse_rpc(payload) {
                Some((rpc_id, body)) => census(
                    true,
                    rpc_id,
                    &body,
                    &mut rpc_stats,
                    &mut pkt_stats,
                    &needles,
                    &mut needle_hits,
                ),
                None => {
                    let Some(&id) = payload.first() else { continue };
                    census(
                        false,
                        id,
                        &payload[1..],
                        &mut rpc_stats,
                        &mut pkt_stats,
                        &needles,
                        &mut needle_hits,
                    );
                }
            }
        }
    }

    report(&rpc_stats, &pkt_stats, &needles, &needle_hits);
}

/// Tally one message into the RPC/packet stats and scan its body for each needle.
#[allow(clippy::too_many_arguments)]
fn census(
    is_rpc: bool,
    id: u8,
    body: &[u8],
    rpc_stats: &mut BTreeMap<u8, Stat>,
    pkt_stats: &mut BTreeMap<u8, Stat>,
    needles: &[i32],
    needle_hits: &mut HashMap<i32, BTreeMap<String, u32>>,
) {
    let stats = if is_rpc { rpc_stats } else { pkt_stats };
    let st = stats.entry(id).or_insert(Stat {
        count: 0,
        total: 0,
        max: 0,
    });
    st.count += 1;
    st.total += body.len() as u64;
    st.max = st.max.max(body.len());
    let kind = if is_rpc {
        format!("rpc {id}")
    } else {
        format!("pkt {id}")
    };
    for &n in needles {
        let le = n.to_le_bytes();
        if body.windows(4).any(|w| w == le) {
            *needle_hits
                .entry(n)
                .or_default()
                .entry(kind.clone())
                .or_insert(0) += 1;
        }
    }
}

/// Print the RPC/packet census and the needle-hit summary.
fn report(
    rpc_stats: &BTreeMap<u8, Stat>,
    pkt_stats: &BTreeMap<u8, Stat>,
    needles: &[i32],
    needle_hits: &HashMap<i32, BTreeMap<String, u32>>,
) {
    println!("--- RPCs ---");
    for (id, s) in rpc_stats {
        println!(
            "rpc {id:>3}: {:>6}x total={:>9}B max={}B",
            s.count, s.total, s.max
        );
    }
    println!("--- raw packets ---");
    for (id, s) in pkt_stats {
        println!(
            "pkt {id:>3}: {:>6}x total={:>9}B max={}B",
            s.count, s.total, s.max
        );
    }
    if !needles.is_empty() {
        println!("--- needle hits (LE i32 anywhere in a body) ---");
        for n in needles {
            match needle_hits.get(n) {
                Some(map) => {
                    let parts: Vec<String> = map.iter().map(|(k, c)| format!("{k} x{c}")).collect();
                    println!("{n}: {}", parts.join(", "));
                }
                None => println!("{n}: not found"),
            }
        }
    }
}
