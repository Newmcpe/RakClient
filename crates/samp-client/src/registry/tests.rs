use super::*;
use std::cell::Cell;
use std::rc::Rc;

#[test]
fn pass_when_no_handlers() {
    let registry = PacketRegistry::new();
    assert_eq!(
        registry.dispatch_rpc(Direction::Incoming, 1, &[1, 2, 3]),
        Verdict::Pass
    );
}

#[test]
fn id_handler_only_fires_for_its_id_and_direction() {
    let hits = Rc::new(Cell::new(0));
    let h = hits.clone();
    let mut registry = PacketRegistry::new();
    registry.on_rpc(Direction::Incoming, 7, move |_, _, _| {
        h.set(h.get() + 1);
        Verdict::Pass
    });
    registry.dispatch_rpc(Direction::Incoming, 7, &[]);
    registry.dispatch_rpc(Direction::Incoming, 8, &[]); // wrong id
    registry.dispatch_rpc(Direction::Outgoing, 7, &[]); // wrong direction
    assert_eq!(hits.get(), 1);
}

#[test]
fn first_drop_wins() {
    let mut registry = PacketRegistry::new();
    registry.on_any_rpc(|_, _, _| Verdict::Drop);
    registry.on_any_rpc(|_, _, _| panic!("should not run after Drop"));
    assert_eq!(
        registry.dispatch_rpc(Direction::Incoming, 1, &[]),
        Verdict::Drop
    );
}

#[test]
fn rewrites_chain() {
    let mut registry = PacketRegistry::new();
    registry.on_any_rpc(|_, _, _| Verdict::Rewrite(vec![1]));
    // second handler sees the rewritten body and appends to it
    registry.on_any_rpc(|_, _, body| {
        let mut v = body.to_vec();
        v.push(2);
        Verdict::Rewrite(v)
    });
    assert_eq!(
        registry.dispatch_rpc(Direction::Incoming, 1, &[0]),
        Verdict::Rewrite(vec![1, 2])
    );
}

#[test]
fn typed_register_decodes_struct_and_rewrites() {
    use samp_proto::events::incoming::PlayerJoin;

    let seen = Rc::new(Cell::new(0u16));
    let s = seen.clone();
    let mut registry = PacketRegistry::new();
    // register by event type — handler receives the decoded struct, returns a typed Action.
    registry.register::<PlayerJoin>(move |mut ev| {
        s.set(ev.player_id);
        ev.player_id = 99;
        Action::Rewrite(ev)
    });

    // PlayerJoin (137): player_id u16=7, color i32=-1, is_npc bool8=false, nickname str8="Bo".
    let payload = [0x07, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x02, b'B', b'o'];
    let verdict = registry.dispatch_rpc(Direction::Incoming, 137, &payload);
    assert_eq!(seen.get(), 7, "handler saw the decoded player_id");
    match verdict {
        Verdict::Rewrite(bytes) => assert_eq!(&bytes[0..2], &[99, 0], "player_id rewritten"),
        other => panic!("expected rewrite, got {other:?}"),
    }
    // The type carries its own direction: registering it routed to the incoming table only.
    assert_eq!(
        registry.dispatch_rpc(Direction::Outgoing, 137, &payload),
        Verdict::Pass
    );
}

#[test]
fn update_handlers_tick() {
    let count = Rc::new(Cell::new(0));
    let c = count.clone();
    let mut registry = PacketRegistry::new();
    assert!(!registry.wants_update());
    registry.on_update(move || c.set(c.get() + 1));
    assert!(registry.wants_update());
    registry.tick();
    registry.tick();
    assert_eq!(count.get(), 2);
}
