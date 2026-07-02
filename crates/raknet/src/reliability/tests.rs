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

/// Reproduces the "server floods hundreds of small ReliableOrdered RPCs in under a second"
/// scenario from the Arizona post-login burst: many small packets on one ordering channel,
/// delivered to the receiver out of the order they were sent (real UDP gives no ordering
/// guarantee). Every payload must still arrive exactly once, fully in-order.
#[test]
fn burst_of_many_ordered_packets_delivers_all_without_gaps() {
    let mut tx = ReliabilityLayer::new();
    let mut rx = ReliabilityLayer::new();
    let now = Instant::now();

    const N: usize = 1500;
    for i in 0..N as u32 {
        tx.enqueue(&i.to_le_bytes(), Reliability::ReliableOrdered, 0);
    }
    let datagrams = tx.update(now);
    assert!(
        datagrams.len() > 1,
        "burst of {N} small packets should span multiple datagrams"
    );

    // Simulate real-world UDP reordering within the burst by reversing small runs of
    // datagrams rather than the whole stream (closer to how a flood actually reorders).
    let mut shuffled: Vec<&Vec<u8>> = datagrams.iter().collect();
    for chunk in shuffled.chunks_mut(7) {
        chunk.reverse();
    }

    let mut delivered = Vec::new();
    for dg in shuffled {
        delivered.extend(rx.on_receive(dg, now).expect("valid datagram"));
    }

    let expected: Vec<Vec<u8>> = (0..N as u32).map(|i| i.to_le_bytes().to_vec()).collect();
    assert_eq!(
        delivered, expected,
        "every packet in the burst must be delivered exactly once, in order"
    );
}

/// The investigator's exact suspect scenario: a burst of many small ReliableOrdered packets
/// followed by one more packet that must still be delivered (i.e. the burst must not leave the
/// ordering-channel bookkeeping in a state where the next legitimate packet is lost/stuck).
#[test]
fn packet_immediately_after_burst_is_still_delivered() {
    let mut tx = ReliabilityLayer::new();
    let mut rx = ReliabilityLayer::new();
    let now = Instant::now();

    const N: usize = 800;
    for i in 0..N as u32 {
        tx.enqueue(&i.to_le_bytes(), Reliability::ReliableOrdered, 0);
    }
    let mut delivered = pump(&mut tx, &mut rx, now);
    assert_eq!(delivered.len(), N, "burst itself must fully deliver");

    tx.enqueue(b"final-after-burst", Reliability::ReliableOrdered, 0);
    delivered.extend(pump(&mut tx, &mut rx, now));

    assert_eq!(delivered.len(), N + 1);
    assert_eq!(
        delivered.last().unwrap().as_slice(),
        b"final-after-burst",
        "the packet right after the burst must still be delivered"
    );
}

/// A large (split/reassembled) message interleaved among many small ordered packets on the same
/// channel must not stall delivery of the packets around it, even when the whole run arrives
/// out of send order — matching a burst that mixes small streamer RPCs with an occasional large
/// one on the same ordering channel.
#[test]
fn split_packet_amid_ordered_burst_does_not_stall_channel() {
    let mut tx = ReliabilityLayer::new();
    let mut rx = ReliabilityLayer::new();
    let now = Instant::now();

    for i in 0u8..20 {
        tx.enqueue(&[i], Reliability::ReliableOrdered, 0);
    }
    let big: Vec<u8> = (0..(SPLIT_FRAGMENT_SIZE * 2 + 5))
        .map(|i| (i % 251) as u8)
        .collect();
    tx.enqueue(&big, Reliability::ReliableOrdered, 0);
    for i in 20u8..40 {
        tx.enqueue(&[i], Reliability::ReliableOrdered, 0);
    }

    let datagrams = tx.update(now);
    assert!(
        datagrams.len() > 1,
        "split payload spans multiple datagrams"
    );

    // Deliver in reverse send order: the split fragments' completing datagram arrives first,
    // the earliest small packets arrive last.
    let mut delivered = Vec::new();
    for dg in datagrams.into_iter().rev() {
        delivered.extend(rx.on_receive(&dg, now).expect("valid datagram"));
    }

    let mut expected: Vec<Vec<u8>> = (0u8..20).map(|i| vec![i]).collect();
    expected.push(big);
    expected.extend((20u8..40).map(|i| vec![i]));
    assert_eq!(delivered, expected);
}

/// One datagram genuinely lost inside a burst (real packet loss, not reordering) must open a
/// *temporary* ordering hole that the RTO resend fills — never a permanent one that blocks every
/// later message on the channel forever.
#[test]
fn dropped_datagram_in_burst_resend_fills_hole_not_permanent() {
    let mut tx = ReliabilityLayer::new();
    let mut rx = ReliabilityLayer::new();
    let t0 = Instant::now();

    const N: usize = 100;
    const DROPPED_INDEX: usize = 50;
    let mut datagrams = Vec::new();
    for i in 0..N as u32 {
        tx.enqueue(&i.to_le_bytes(), Reliability::ReliableOrdered, 0);
        datagrams.extend(tx.update(t0));
    }
    assert_eq!(
        datagrams.len(),
        N,
        "one packet per datagram for a clean drop simulation"
    );

    let mut delivered = Vec::new();
    for (idx, dg) in datagrams.iter().enumerate() {
        if idx == DROPPED_INDEX {
            continue; // simulate real packet loss, not just reordering
        }
        delivered.extend(rx.on_receive(dg, t0).expect("valid datagram"));
    }
    assert_eq!(
        delivered.len(),
        DROPPED_INDEX,
        "everything after the hole must be buffered, not delivered out of order"
    );

    // Ack back what rx actually received so tx's resend queue reflects the true loss.
    for dg in rx.update(t0) {
        let _ = tx.on_receive(&dg, t0).expect("valid ack");
    }
    assert_eq!(
        tx.resend_len(),
        1,
        "only the genuinely dropped message should remain unacked"
    );

    let t1 = t0 + INITIAL_RTO + Duration::from_millis(1);
    let resent = tx.update(t1);
    assert_eq!(resent.len(), 1, "RTO fires exactly one resend for the hole");
    for dg in resent {
        delivered.extend(rx.on_receive(&dg, t1).expect("valid datagram"));
    }

    let expected: Vec<Vec<u8>> = (0..N as u32).map(|i| i.to_le_bytes().to_vec()).collect();
    assert_eq!(
        delivered, expected,
        "resend must fill the hole and flush every buffered packet after it; no permanent stall"
    );
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

fn single_packet_datagram(pkt: &OutPacket, message_number: u32) -> Vec<u8> {
    let mut w = BitStreamWriter::new();
    w.write_bit(false); // hasAcks = 0 (data datagram)
    pkt.encode(message_number, &mut w);
    w.into_bytes()
}

/// A capacity-rejected split fragment's `messageNumber` is still unconditionally ACKed (so a
/// real sender never resends it), yet its payload is thrown away -- and if that fragment was
/// the next expected `ReliableOrdered` delivery on its channel, every later ordered message on
/// that channel is buffered forever because the hole it left behind can never fill.
#[test]
fn split_capacity_reject_acks_dropped_fragment_and_stalls_ordering_channel() {
    let mut tx = ReliabilityLayer::new();
    let mut rx = ReliabilityLayer::new();
    let now = Instant::now();

    let payload: Vec<u8> = (0..(SPLIT_FRAGMENT_SIZE * 2))
        .map(|i| (i % 251) as u8)
        .collect();

    // Enqueue the message that will occupy ordering_index 0 (the very next expected delivery)
    // first, but withhold its fragments from `rx` until capacity is already exhausted.
    tx.enqueue(&payload, Reliability::ReliableOrdered, 0);
    let withheld = tx.update(now);
    assert_eq!(
        withheld.len(),
        2,
        "full-size fragments land in separate datagrams"
    );

    // Fill every concurrent-split slot with other, independently-splitting messages (ordering
    // indices 1..=32), so `rx` is already at MAX_CONCURRENT_SPLITS when the withheld message's
    // first fragment finally arrives.
    let mut filler_first_fragments = Vec::new();
    for _ in 0..MAX_CONCURRENT_SPLITS {
        tx.enqueue(&payload, Reliability::ReliableOrdered, 0);
        let datagrams = tx.update(now);
        assert_eq!(datagrams.len(), 2);
        filler_first_fragments.push(datagrams[0].clone());
    }
    for dg in &filler_first_fragments {
        let delivered = rx.on_receive(dg, now).expect("valid");
        assert!(delivered.is_empty());
    }
    assert_eq!(
        rx.splits.len(),
        MAX_CONCURRENT_SPLITS,
        "capacity is exhausted by incomplete splits"
    );
    let ids_before: std::collections::BTreeSet<u16> = rx.splits.keys().copied().collect();

    // The ordering_index-0 message's first fragment now arrives as a brand-new split id while
    // capacity is full.
    let ack_count_before = rx.ack_queue.len();
    let delivered = rx.on_receive(&withheld[0], now).expect("valid");
    assert!(
        delivered.is_empty(),
        "capacity-rejected fragment cannot be delivered"
    );
    assert_eq!(
        rx.ack_queue.len(),
        ack_count_before + 1,
        "dropped fragment's message number was still acked"
    );
    let ids_after: std::collections::BTreeSet<u16> = rx.splits.keys().copied().collect();
    assert_eq!(
        ids_after, ids_before,
        "capacity-rejected split id never actually got a slot"
    );

    // Any later ReliableOrdered message on the same channel is now buffered forever: the hole
    // at ordering_index 0 can never fill, because the fragment that would fill it was already
    // silently discarded (and, per the ack above, will never be resent by a real sender).
    tx.enqueue(
        b"SetSpawnInfo-like payload",
        Reliability::ReliableOrdered,
        0,
    );
    let next_msg = tx.update(now);
    assert_eq!(next_msg.len(), 1);
    let delivered = rx.on_receive(&next_msg[0], now).expect("valid");
    assert!(
        !delivered.is_empty(),
        "BUG: a later ReliableOrdered message must not be permanently stuck behind a \
         silently-dropped, already-acked split fragment (this is the SetSpawnInfo-after-burst \
         stall)"
    );
    assert_ne!(
        rx.recv_ordering_index[0], 0,
        "BUG: expected ordering index must eventually advance past the hole"
    );
    assert!(
        !rx.ordering_heap[0].contains_key(&33),
        "BUG: later message must not remain stuck in the ordering heap forever"
    );
}

/// A split id reused (by wire header, not by `ReliabilityLayer`'s own counter) for an unrelated
/// message while an earlier incomplete buffer with that id is still resident silently discards
/// the earlier buffer -- including a fragment whose message number was already ACKed -- and can
/// leave the same kind of permanent ordering-channel hole as the capacity-rejection path above.
/// (Low real-world likelihood: `split_id_counter` only repeats after 65536 in-flight splits from
/// one sender, far beyond a single post-login burst -- included because the mechanism itself is
/// concretely reproducible and shares the same silent-ack/permanent-hole failure mode.)
#[test]
fn split_id_reuse_silently_discards_incomplete_buffer() {
    let mut rx = ReliabilityLayer::new();
    let now = Instant::now();

    let pkt_a = OutPacket {
        reliability: Reliability::ReliableOrdered,
        ordering_channel: 0,
        ordering_index: 0,
        split: Some(SplitInfo {
            id: 7,
            index: 0,
            count: 2,
        }),
        payload: vec![0xAAu8; 4],
    };
    let dg_a = single_packet_datagram(&pkt_a, 0);
    let delivered = rx.on_receive(&dg_a, now).expect("valid");
    assert!(delivered.is_empty());
    assert_eq!(rx.splits.len(), 1);
    assert_eq!(rx.splits[&7].fragments.len(), 1);

    // A different message reuses split id 7 before A's reassembly completes, with a different
    // fragment count -- exactly what a wrapped `split_id_counter` would produce.
    let pkt_b = OutPacket {
        reliability: Reliability::ReliableOrdered,
        ordering_channel: 0,
        ordering_index: 1,
        split: Some(SplitInfo {
            id: 7,
            index: 0,
            count: 3,
        }),
        payload: vec![0xBBu8; 4],
    };
    let dg_b = single_packet_datagram(&pkt_b, 1);
    let ack_count_before = rx.ack_queue.len();
    let delivered = rx.on_receive(&dg_b, now).expect("valid");
    assert!(delivered.is_empty());
    assert_eq!(
        rx.ack_queue.len(),
        ack_count_before + 1,
        "message B's number was still acked"
    );
    assert!(
        !rx.splits.contains_key(&7),
        "colliding id wipes A's incomplete buffer, including its already-acked fragment"
    );

    // A's remaining fragment can no longer complete the original message: the buffer that would
    // have held it is gone, and A's first fragment will never be resent (it was already acked).
    let pkt_a2 = OutPacket {
        reliability: Reliability::ReliableOrdered,
        ordering_channel: 0,
        ordering_index: 0,
        split: Some(SplitInfo {
            id: 7,
            index: 1,
            count: 2,
        }),
        payload: vec![0xAAu8; 4],
    };
    let dg_a2 = single_packet_datagram(&pkt_a2, 2);
    let delivered = rx.on_receive(&dg_a2, now).expect("valid");
    assert!(
        delivered.is_empty(),
        "A's reassembly can never complete once its buffer was wiped"
    );

    // The next ReliableOrdered message on the channel is stuck forever behind the hole at
    // ordering_index 0.
    let pkt_c = OutPacket {
        reliability: Reliability::ReliableOrdered,
        ordering_channel: 0,
        ordering_index: 2,
        split: None,
        payload: b"later message".to_vec(),
    };
    let dg_c = single_packet_datagram(&pkt_c, 3);
    let delivered = rx.on_receive(&dg_c, now).expect("valid");
    assert!(
        !delivered.is_empty(),
        "BUG: a later ReliableOrdered message must not be permanently stuck behind a hole \
         left by a split-id collision that silently discarded an already-acked fragment"
    );
    assert_ne!(rx.recv_ordering_index[0], 0);
    assert!(!rx.ordering_heap[0].contains_key(&2));
}
