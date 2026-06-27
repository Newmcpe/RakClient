//! RakNet 3.x (SA-MP / open.mp "legacy network") reliability layer.
//!
//! Operates on *plaintext* datagrams (the [`crate::cipher`] wraps each datagram on the wire). The
//! on-wire encoding is the bit-packed RakNet 3.x format:
//!
//! * every datagram starts with a 1-bit `hasAcks` flag; if set, the rest is an ACK range list, if
//!   clear, the rest is a sequence of internal packets;
//! * each internal packet carries a 16-bit `messageNumber`, a 4-bit reliability field (SA-MP's
//!   `PacketReliability` enum is offset by 6, so the wire values are 6..=10), an optional 5-bit
//!   ordering channel + 16-bit ordering index, a split flag (+ split header), a compressed data bit
//!   length, then byte-aligned payload bytes.
//!
//! Acknowledgements acknowledge `messageNumber`s; the sender resends unacked reliable packets after
//! an RTO. `messageNumber`/`orderingIndex` are 16-bit on the wire; this implementation widens them
//! to `u32` for bookkeeping and does not model 16-bit wraparound (a session must stay under 65 536
//! outstanding messages, which the connect → play handshake never approaches).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use samp_proto::{BitStreamReader, BitStreamWriter};

use crate::{Reliability, Result};

pub(crate) const NUM_ORDERING_CHANNELS: usize = 32;

/// Plaintext budget for a single datagram (kept under a 1492-byte MTU minus IP/UDP and the cipher's
/// one checksum byte).
pub(crate) const MAX_DATAGRAM_PAYLOAD: usize = 1400;

/// Largest application payload carried in one (non-split) internal packet before we fragment.
const SPLIT_FRAGMENT_SIZE: usize = MAX_DATAGRAM_PAYLOAD - 64;

const INITIAL_RTO: Duration = Duration::from_millis(1000);
const MIN_RTO: Duration = Duration::from_millis(100);
const MAX_RTO: Duration = Duration::from_millis(3000);

/// Sliding window (in message numbers) used to bound dedup bookkeeping and reject ancient ids.
const DEDUP_WINDOW: u32 = 4096;
/// How far ahead of the next expected ordered index we are willing to buffer before dropping.
const ORDER_WINDOW: u32 = 4096;

/// SA-MP `PacketReliability` enum offset: the wire 4-bit field is `Reliability as u8 + 6`.
const RELIABILITY_WIRE_BASE: u8 = 6;

fn to_wire(reliability: Reliability) -> u8 {
    reliability as u8 + RELIABILITY_WIRE_BASE
}

fn from_wire(value: u8) -> Option<Reliability> {
    value
        .checked_sub(RELIABILITY_WIRE_BASE)
        .and_then(Reliability::from_u8)
}

/// Wire reliability values that carry an ordering channel + index (UNRELIABLE_SEQUENCED = 7,
/// RELIABLE_ORDERED = 9, RELIABLE_SEQUENCED = 10).
fn wire_has_ordering(value: u8) -> bool {
    matches!(value, 7 | 9 | 10)
}

impl Reliability {
    pub(crate) fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Reliability::Unreliable),
            1 => Some(Reliability::UnreliableSequenced),
            2 => Some(Reliability::Reliable),
            3 => Some(Reliability::ReliableOrdered),
            4 => Some(Reliability::ReliableSequenced),
            _ => None,
        }
    }

    pub(crate) fn is_reliable(self) -> bool {
        matches!(
            self,
            Reliability::Reliable | Reliability::ReliableOrdered | Reliability::ReliableSequenced
        )
    }

    pub(crate) fn is_ordered(self) -> bool {
        matches!(self, Reliability::ReliableOrdered)
    }

    pub(crate) fn is_sequenced(self) -> bool {
        matches!(
            self,
            Reliability::UnreliableSequenced | Reliability::ReliableSequenced
        )
    }

    fn has_ordering(self) -> bool {
        self.is_ordered() || self.is_sequenced()
    }
}

#[derive(Clone, Copy)]
struct SplitInfo {
    id: u16,
    index: u32,
    count: u32,
}

/// A packet queued for (or awaiting re-) transmission. The 16-bit `messageNumber` is assigned only
/// when the packet is written into a datagram, so resends get a fresh number.
#[derive(Clone)]
struct OutPacket {
    reliability: Reliability,
    ordering_channel: u8,
    ordering_index: u32,
    split: Option<SplitInfo>,
    payload: Vec<u8>,
}

impl OutPacket {
    fn encoded_len_estimate(&self) -> usize {
        2 + 1 + 3 + 1 + if self.split.is_some() { 11 } else { 0 } + 3 + 1 + self.payload.len()
    }

    fn encode(&self, message_number: u32, w: &mut BitStreamWriter) {
        w.write_u16(message_number as u16);
        let wire = to_wire(self.reliability);
        w.write_bits_low(wire, 4);
        if self.reliability.has_ordering() {
            w.write_bits_low(self.ordering_channel, 5);
            w.write_u16(self.ordering_index as u16);
        }
        match self.split {
            Some(s) => {
                w.write_bit(true);
                w.write_u16(s.id);
                w.write_compressed_u32(s.index);
                w.write_compressed_u32(s.count);
            }
            None => w.write_bit(false),
        }
        let data_bits = (self.payload.len() * 8) as u16;
        w.write_compressed_u16(data_bits);
        w.align_to_byte();
        w.write_bytes(&self.payload);
    }
}

struct DecodedPacket {
    message_number: u32,
    reliability: Reliability,
    ordering_channel: u8,
    ordering_index: u32,
    split: Option<SplitInfo>,
    payload: Vec<u8>,
}

fn decode_packet(r: &mut BitStreamReader<'_>) -> Result<DecodedPacket> {
    let message_number = r.read_u16()? as u32;
    let wire = r.read_bits_low(4)?;
    let reliability = from_wire(wire).ok_or(crate::RaknetError::Malformed)?;
    let (ordering_channel, ordering_index) = if wire_has_ordering(wire) {
        let ch = r.read_bits_low(5)?;
        if ch as usize >= NUM_ORDERING_CHANNELS {
            return Err(crate::RaknetError::Malformed);
        }
        (ch, r.read_u16()? as u32)
    } else {
        (0, 0)
    };
    let split = if r.read_bit()? {
        Some(SplitInfo {
            id: r.read_u16()?,
            index: r.read_compressed_u32()?,
            count: r.read_compressed_u32()?,
        })
    } else {
        None
    };
    let data_bits = r.read_compressed_u16()? as usize;
    r.align_to_byte();
    let payload = r.read_bytes(data_bits.div_ceil(8))?;
    tracing::trace!(
        message_number,
        wire,
        ordering_channel,
        ordering_index,
        split = split.is_some(),
        data_bits,
        payload_len = payload.len(),
        "decoded packet"
    );
    Ok(DecodedPacket {
        message_number,
        reliability,
        ordering_channel,
        ordering_index,
        split,
        payload,
    })
}

struct ResendEntry {
    packet: OutPacket,
    last_sent: Instant,
    resent: bool,
}

struct SplitBuffer {
    count: u32,
    reliability: Reliability,
    ordering_channel: u8,
    ordering_index: u32,
    fragments: BTreeMap<u32, Vec<u8>>,
}

/// The reliability state machine for one connection (one peer).
pub struct ReliabilityLayer {
    next_message_number: u32,
    send_ordering_index: [u32; NUM_ORDERING_CHANNELS],
    split_id_counter: u16,
    send_queue: VecDeque<OutPacket>,
    resend: BTreeMap<u32, ResendEntry>,

    ack_queue: BTreeSet<u32>,
    received_messages: BTreeSet<u32>,
    highest_received: Option<u32>,
    recv_ordering_index: [u32; NUM_ORDERING_CHANNELS],
    ordering_heap: Vec<BTreeMap<u32, Vec<u8>>>,
    recv_sequenced_index: [Option<u32>; NUM_ORDERING_CHANNELS],
    splits: BTreeMap<u16, SplitBuffer>,

    srtt: Option<Duration>,
    rttvar: Duration,
    rto: Duration,
}

impl Default for ReliabilityLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl ReliabilityLayer {
    pub fn new() -> Self {
        ReliabilityLayer {
            next_message_number: 0,
            send_ordering_index: [0; NUM_ORDERING_CHANNELS],
            split_id_counter: 0,
            send_queue: VecDeque::new(),
            resend: BTreeMap::new(),
            ack_queue: BTreeSet::new(),
            received_messages: BTreeSet::new(),
            highest_received: None,
            recv_ordering_index: [0; NUM_ORDERING_CHANNELS],
            ordering_heap: (0..NUM_ORDERING_CHANNELS)
                .map(|_| BTreeMap::new())
                .collect(),
            recv_sequenced_index: [None; NUM_ORDERING_CHANNELS],
            splits: BTreeMap::new(),
            srtt: None,
            rttvar: Duration::ZERO,
            rto: INITIAL_RTO,
        }
    }

    #[cfg(test)]
    pub(crate) fn resend_len(&self) -> usize {
        self.resend.len()
    }

    /// Queue an application payload for transmission, fragmenting if it exceeds the MTU budget.
    pub fn enqueue(&mut self, payload: &[u8], reliability: Reliability, channel: u8) {
        let channel = (channel as usize % NUM_ORDERING_CHANNELS) as u8;
        let ordering_index = if reliability.has_ordering() {
            let idx = self.send_ordering_index[channel as usize];
            self.send_ordering_index[channel as usize] = idx.wrapping_add(1);
            idx
        } else {
            0
        };

        if payload.len() <= SPLIT_FRAGMENT_SIZE {
            self.send_queue.push_back(OutPacket {
                reliability,
                ordering_channel: channel,
                ordering_index,
                split: None,
                payload: payload.to_vec(),
            });
            return;
        }

        let id = self.split_id_counter;
        self.split_id_counter = self.split_id_counter.wrapping_add(1);
        let chunks: Vec<&[u8]> = payload.chunks(SPLIT_FRAGMENT_SIZE).collect();
        let count = chunks.len() as u32;
        for (index, chunk) in chunks.into_iter().enumerate() {
            self.send_queue.push_back(OutPacket {
                reliability,
                ordering_channel: channel,
                ordering_index,
                split: Some(SplitInfo {
                    id,
                    index: index as u32,
                    count,
                }),
                payload: chunk.to_vec(),
            });
        }
    }

    fn take_message_number(&mut self) -> u32 {
        let n = self.next_message_number;
        self.next_message_number = self.next_message_number.wrapping_add(1) & 0xFFFF;
        n
    }

    /// Produce every datagram that should go on the wire now: pending ACK ranges, freshly queued
    /// messages, and any reliable datagrams whose RTO has elapsed.
    pub fn update(&mut self, now: Instant) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let acks = std::mem::take(&mut self.ack_queue);
        out.extend(encode_ack_datagrams(&acks));
        self.flush_send_queue(now, &mut out);
        self.flush_resends(now, &mut out);
        out
    }

    fn flush_send_queue(&mut self, now: Instant, out: &mut Vec<Vec<u8>>) {
        while !self.send_queue.is_empty() {
            let mut w = BitStreamWriter::new();
            w.write_bit(false); // hasAcks = 0 (data datagram)
            let mut wrote_any = false;

            while let Some(front) = self.send_queue.front() {
                let projected = w.bit_len() / 8 + front.encoded_len_estimate();
                if wrote_any && projected > MAX_DATAGRAM_PAYLOAD {
                    break;
                }
                let pkt = match self.send_queue.pop_front() {
                    Some(p) => p,
                    None => break,
                };
                let message_number = self.take_message_number();
                pkt.encode(message_number, &mut w);
                wrote_any = true;
                if pkt.reliability.is_reliable() {
                    self.resend.insert(
                        message_number,
                        ResendEntry {
                            packet: pkt,
                            last_sent: now,
                            resent: false,
                        },
                    );
                }
            }

            if wrote_any {
                out.push(w.into_bytes());
            }
        }
    }

    fn flush_resends(&mut self, now: Instant, out: &mut Vec<Vec<u8>>) {
        let due: Vec<u32> = self
            .resend
            .iter()
            .filter(|(_, e)| now.duration_since(e.last_sent) >= self.rto)
            .map(|(mn, _)| *mn)
            .collect();
        if due.is_empty() {
            return;
        }
        for mn in due {
            let mut entry = match self.resend.remove(&mn) {
                Some(e) => e,
                None => continue,
            };
            let new_mn = self.take_message_number();
            let mut w = BitStreamWriter::new();
            w.write_bit(false);
            entry.packet.encode(new_mn, &mut w);
            entry.last_sent = now;
            entry.resent = true;
            self.resend.insert(new_mn, entry);
            out.push(w.into_bytes());
        }
        self.rto = (self.rto * 2).min(MAX_RTO);
    }

    /// Process one decrypted inbound datagram, returning fully-ordered application payloads.
    pub fn on_receive(&mut self, datagram: &[u8], now: Instant) -> Result<Vec<Vec<u8>>> {
        let mut r = BitStreamReader::new(datagram);
        let has_acks = r.read_bit()?;
        if has_acks {
            self.handle_acks(&mut r, now)?;
        }

        // A single datagram may carry both the acknowledgements and one or more internal packets;
        // RakNet stops creating internal packets once fewer than `messageNumber` (16) bits remain.
        let mut delivered = Vec::new();
        while r.bits_left() >= 16 {
            let pkt = match decode_packet(&mut r) {
                Ok(pkt) => pkt,
                Err(_) => break,
            };
            self.process_packet(pkt, &mut delivered);
        }
        Ok(delivered)
    }

    fn handle_acks(&mut self, r: &mut BitStreamReader<'_>, now: Instant) -> Result<()> {
        let count = r.read_compressed_u16()?;
        for _ in 0..count {
            let is_single = r.read_bit()?;
            let min = r.read_u16()? as u32;
            let max = if is_single { min } else { r.read_u16()? as u32 };
            if max < min || max - min > MAX_DATAGRAM_PAYLOAD as u32 {
                return Err(crate::RaknetError::Malformed);
            }
            for mn in min..=max {
                if let Some(entry) = self.resend.remove(&mn) {
                    if !entry.resent {
                        self.sample_rtt(now.duration_since(entry.last_sent));
                    }
                }
            }
        }
        Ok(())
    }

    fn process_packet(&mut self, pkt: DecodedPacket, out: &mut Vec<Vec<u8>>) {
        // Acknowledge every received message number (RakNet acks unconditionally; the sender's
        // resend map only holds reliable ones, so acking unreliable numbers is harmless).
        self.ack_queue.insert(pkt.message_number);

        if self.is_duplicate(pkt.message_number) {
            return;
        }
        self.received_messages.insert(pkt.message_number);
        self.highest_received = Some(
            self.highest_received
                .map_or(pkt.message_number, |h| h.max(pkt.message_number)),
        );
        if let Some(h) = self.highest_received {
            let floor = h.saturating_sub(DEDUP_WINDOW);
            self.received_messages = self.received_messages.split_off(&floor);
        }

        if let Some(s) = pkt.split {
            let buf = self.splits.entry(s.id).or_insert_with(|| SplitBuffer {
                count: s.count,
                reliability: pkt.reliability,
                ordering_channel: pkt.ordering_channel,
                ordering_index: pkt.ordering_index,
                fragments: BTreeMap::new(),
            });
            if s.count == 0 || s.count != buf.count || s.index >= s.count {
                self.splits.remove(&s.id);
                return;
            }
            buf.fragments.insert(s.index, pkt.payload);
            if buf.fragments.len() as u32 == buf.count {
                if let Some(buf) = self.splits.remove(&s.id) {
                    let mut full = Vec::new();
                    for index in 0..buf.count {
                        if let Some(frag) = buf.fragments.get(&index) {
                            full.extend_from_slice(frag);
                        }
                    }
                    self.deliver(
                        buf.reliability,
                        buf.ordering_channel,
                        buf.ordering_index,
                        full,
                        out,
                    );
                }
            }
            return;
        }

        self.deliver(
            pkt.reliability,
            pkt.ordering_channel,
            pkt.ordering_index,
            pkt.payload,
            out,
        );
    }

    fn is_duplicate(&self, mn: u32) -> bool {
        if self.received_messages.contains(&mn) {
            return true;
        }
        if let Some(h) = self.highest_received {
            if u64::from(mn) + u64::from(DEDUP_WINDOW) < u64::from(h) {
                return true;
            }
        }
        false
    }

    fn deliver(
        &mut self,
        reliability: Reliability,
        channel: u8,
        ordering_index: u32,
        payload: Vec<u8>,
        out: &mut Vec<Vec<u8>>,
    ) {
        let ch = channel as usize % NUM_ORDERING_CHANNELS;
        if reliability.is_ordered() {
            let expected = self.recv_ordering_index[ch];
            if ordering_index == expected {
                out.push(payload);
                self.recv_ordering_index[ch] = expected.wrapping_add(1);
                while let Some(next) = self.ordering_heap[ch].remove(&self.recv_ordering_index[ch])
                {
                    out.push(next);
                    self.recv_ordering_index[ch] = self.recv_ordering_index[ch].wrapping_add(1);
                }
            } else if ordering_index > expected
                && ordering_index <= expected.saturating_add(ORDER_WINDOW)
            {
                self.ordering_heap[ch].insert(ordering_index, payload);
            }
        } else if reliability.is_sequenced() {
            let newer = match self.recv_sequenced_index[ch] {
                Some(last) => ordering_index > last,
                None => true,
            };
            if newer {
                self.recv_sequenced_index[ch] = Some(ordering_index);
                out.push(payload);
            }
        } else {
            out.push(payload);
        }
    }

    fn sample_rtt(&mut self, rtt: Duration) {
        match self.srtt {
            None => {
                self.srtt = Some(rtt);
                self.rttvar = rtt / 2;
            }
            Some(s) => {
                let diff = rtt.abs_diff(s);
                self.rttvar = (self.rttvar * 3 + diff) / 4;
                self.srtt = Some((s * 7 + rtt) / 8);
            }
        }
        let srtt = self.srtt.unwrap_or(INITIAL_RTO);
        self.rto = (srtt + self.rttvar * 4).clamp(MIN_RTO, MAX_RTO);
    }
}

fn coalesce(numbers: &BTreeSet<u32>) -> Vec<(u32, u32)> {
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for &n in numbers {
        match ranges.last_mut() {
            Some(last) if n == last.1 + 1 => last.1 = n,
            _ => ranges.push((n, n)),
        }
    }
    ranges
}

fn encode_ack_datagrams(numbers: &BTreeSet<u32>) -> Vec<Vec<u8>> {
    if numbers.is_empty() {
        return Vec::new();
    }
    let ranges = coalesce(numbers);
    // Keep the serialized range list inside one datagram; the handshake never produces enough acks
    // to need more than a single one.
    let max_ranges = (MAX_DATAGRAM_PAYLOAD - 4) / 5;
    let mut datagrams = Vec::new();
    for chunk in ranges.chunks(max_ranges.max(1)) {
        let mut w = BitStreamWriter::new();
        w.write_bit(true); // hasAcks
        w.write_compressed_u16(chunk.len() as u16);
        for &(min, max) in chunk {
            w.write_bit(min == max);
            w.write_u16(min as u16);
            if min != max {
                w.write_u16(max as u16);
            }
        }
        datagrams.push(w.into_bytes());
    }
    datagrams
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pump(from: &mut ReliabilityLayer, to: &mut ReliabilityLayer, now: Instant) -> Vec<Vec<u8>> {
        let mut delivered = Vec::new();
        for dg in from.update(now) {
            delivered.extend(to.on_receive(&dg, now).expect("valid datagram"));
        }
        delivered
    }

    #[test]
    fn ordered_despite_reorder() {
        let mut tx = ReliabilityLayer::new();
        let mut rx = ReliabilityLayer::new();
        let now = Instant::now();

        let mut datagrams = Vec::new();
        for i in 0u8..3 {
            tx.enqueue(&[i], Reliability::ReliableOrdered, 0);
            datagrams.extend(tx.update(now));
        }
        assert_eq!(datagrams.len(), 3);

        let mut delivered = Vec::new();
        for idx in [2usize, 0, 1] {
            delivered.extend(rx.on_receive(&datagrams[idx], now).expect("valid"));
        }
        assert_eq!(delivered, vec![vec![0u8], vec![1u8], vec![2u8]]);
    }

    #[test]
    fn duplicate_reliable_dropped() {
        let mut tx = ReliabilityLayer::new();
        let mut rx = ReliabilityLayer::new();
        let now = Instant::now();

        tx.enqueue(b"hello", Reliability::Reliable, 0);
        let datagrams = tx.update(now);
        assert_eq!(datagrams.len(), 1);

        let first = rx.on_receive(&datagrams[0], now).expect("valid");
        let second = rx.on_receive(&datagrams[0], now).expect("valid");
        assert_eq!(first, vec![b"hello".to_vec()]);
        assert!(second.is_empty(), "duplicate must be dropped");
    }

    #[test]
    fn ack_frees_resend_queue() {
        let mut tx = ReliabilityLayer::new();
        let mut rx = ReliabilityLayer::new();
        let now = Instant::now();

        tx.enqueue(b"reliable", Reliability::Reliable, 0);
        let _ = pump(&mut tx, &mut rx, now);
        assert_eq!(tx.resend_len(), 1, "reliable message is tracked for resend");

        let _ = pump(&mut rx, &mut tx, now);
        assert_eq!(tx.resend_len(), 0, "ACK frees the resend queue");
    }

    #[test]
    fn rto_resend_until_acked() {
        let mut tx = ReliabilityLayer::new();
        let mut rx = ReliabilityLayer::new();
        let t0 = Instant::now();

        tx.enqueue(b"reliable", Reliability::Reliable, 0);
        let first = tx.update(t0);
        assert_eq!(first.len(), 1);
        let later = t0 + INITIAL_RTO + Duration::from_millis(1);
        let resent = tx.update(later);
        assert_eq!(
            resent.len(),
            1,
            "unacked reliable datagram is resent after RTO"
        );

        for dg in resent {
            let _ = rx.on_receive(&dg, later).expect("valid");
        }
        let _ = pump(&mut rx, &mut tx, later);
        assert_eq!(tx.resend_len(), 0);
    }

    #[test]
    fn oversize_splits_and_reassembles() {
        let mut tx = ReliabilityLayer::new();
        let mut rx = ReliabilityLayer::new();
        let now = Instant::now();

        let payload: Vec<u8> = (0..(SPLIT_FRAGMENT_SIZE * 3 + 17))
            .map(|i| (i % 251) as u8)
            .collect();
        tx.enqueue(&payload, Reliability::ReliableOrdered, 0);
        let datagrams = tx.update(now);
        assert!(
            datagrams.len() >= 2,
            "payload split into multiple datagrams"
        );

        let mut delivered = Vec::new();
        for dg in datagrams.into_iter().rev() {
            delivered.extend(rx.on_receive(&dg, now).expect("valid"));
        }
        assert_eq!(delivered, vec![payload]);
    }

    #[test]
    fn unreliable_sequenced_drops_stale() {
        let mut tx = ReliabilityLayer::new();
        let mut rx = ReliabilityLayer::new();
        let now = Instant::now();

        let mut datagrams = Vec::new();
        for i in 0u8..3 {
            tx.enqueue(&[i], Reliability::UnreliableSequenced, 1);
            datagrams.extend(tx.update(now));
        }

        let mut delivered = Vec::new();
        for idx in [2usize, 0, 1] {
            delivered.extend(rx.on_receive(&datagrams[idx], now).expect("valid"));
        }
        assert_eq!(delivered, vec![vec![2u8]]);
    }

    #[test]
    fn multiple_packets_pack_into_one_datagram() {
        let mut tx = ReliabilityLayer::new();
        let mut rx = ReliabilityLayer::new();
        let now = Instant::now();

        tx.enqueue(&[1, 2, 3], Reliability::Reliable, 0);
        tx.enqueue(&[4, 5], Reliability::ReliableOrdered, 0);
        tx.enqueue(&[6], Reliability::Unreliable, 0);
        let datagrams = tx.update(now);
        assert_eq!(
            datagrams.len(),
            1,
            "small packets coalesce into one datagram"
        );

        let delivered = rx.on_receive(&datagrams[0], now).expect("valid");
        assert_eq!(delivered, vec![vec![1, 2, 3], vec![4, 5], vec![6]]);
    }

    #[test]
    fn ack_round_trips_message_numbers() {
        let mut rx = ReliabilityLayer::new();
        let now = Instant::now();
        let mut tx = ReliabilityLayer::new();
        for i in 0u8..4 {
            tx.enqueue(&[i], Reliability::Reliable, 0);
        }
        for dg in tx.update(now) {
            let _ = rx.on_receive(&dg, now).expect("valid");
        }
        let acks = rx.update(now);
        assert_eq!(acks.len(), 1, "all acks coalesce into one datagram");
        // Feeding the ack back clears tx's resend queue.
        for dg in acks {
            let _ = tx.on_receive(&dg, now).expect("valid ack");
        }
        assert_eq!(tx.resend_len(), 0);
    }
}
