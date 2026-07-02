//! Incoming (server→client) RPC events with fixed field layouts, ported from `INCOMING_RPCS` in
//! `samp/events.lua`, grouped by domain.
//!
//! Content-dependent incoming RPCs (`onInitGame`, `onInitMenu`, `onCreateObject`,
//! `onSetObjectMaterial(Text)`, `onUpdateScoresAndPings`, vehicle stream-in) carry nested or
//! end-of-stream data and are hand-written elsewhere rather than via [`samp_event!`].

pub mod actor;
pub mod camera;
pub mod object;
pub mod player;
pub mod session;
pub mod ui;
pub mod vehicle;
pub mod world;

pub use actor::*;
pub use camera::*;
pub use object::*;
pub use player::*;
pub use session::*;
pub use ui::*;
pub use vehicle::*;
pub use world::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{decode_event, encode_event};
    use crate::Vector3;

    #[test]
    fn player_join_roundtrips() {
        let ev = PlayerJoin {
            player_id: 7,
            color: -1,
            is_npc: false,
            nickname: b"Tester".to_vec(),
        };
        let bytes = encode_event(&ev);
        let back: PlayerJoin = decode_event(&bytes).unwrap();
        assert_eq!(ev, back);
        assert_eq!(PlayerJoin::RPC_ID, 137);
        assert_eq!(PlayerJoin::EVENT, "onPlayerJoin");
    }

    #[test]
    fn stream_in_roundtrips_vectors() {
        let ev = PlayerStreamIn {
            player_id: 3,
            team: 1,
            model: 0,
            position: Vector3 {
                x: 10.0,
                y: -20.5,
                z: 3.25,
            },
            rotation: 90.0,
            color: 0x00FF00FF,
            fighting_style: 4,
        };
        let bytes = encode_event(&ev);
        assert_eq!(decode_event::<PlayerStreamIn>(&bytes).unwrap(), ev);
    }

    #[test]
    fn empty_body_event_roundtrips() {
        let ev = ResetPlayerWeapons {};
        let bytes = encode_event(&ev);
        assert!(bytes.is_empty());
        assert_eq!(decode_event::<ResetPlayerWeapons>(&bytes).unwrap(), ev);
    }

    // Golden-byte tests: the exact wire layout is hand-computed from the SA-MP field spec (LE ints,
    // `str8` = u8 length + bytes, `str32` = u32 length + bytes, `vector3` = 3×f32). Unlike the
    // round-trip tests these are an *independent* oracle — a field decoded at the wrong width or in
    // the wrong order fails here even though it would round-trip cleanly through the macro.

    #[test]
    fn player_join_golden_bytes() {
        let ev = PlayerJoin {
            player_id: 7,
            color: -1,
            is_npc: false,
            nickname: b"Bo".to_vec(),
        };
        let expected = vec![
            0x07, 0x00, // player_id u16 = 7
            0xFF, 0xFF, 0xFF, 0xFF, // color i32 = -1
            0x00, // is_npc bool8 = false
            0x02, b'B', b'o', // nickname str8 = "Bo"
        ];
        assert_eq!(encode_event(&ev), expected);
        assert_eq!(decode_event::<PlayerJoin>(&expected).unwrap(), ev);
    }

    #[test]
    fn set_race_checkpoint_golden_bytes() {
        let ev = SetRaceCheckpoint {
            kind: 1,
            position: Vector3 {
                x: 1.0,
                y: 2.0,
                z: 3.0,
            },
            next_position: Vector3 {
                x: 4.0,
                y: 5.0,
                z: 6.0,
            },
            size: 0.5,
        };
        let expected = vec![
            0x01, // kind u8
            0x00, 0x00, 0x80, 0x3F, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x40,
            0x40, // pos 1,2,3
            0x00, 0x00, 0x80, 0x40, 0x00, 0x00, 0xA0, 0x40, 0x00, 0x00, 0xC0,
            0x40, // next 4,5,6
            0x00, 0x00, 0x00, 0x3F, // size 0.5
        ];
        assert_eq!(encode_event(&ev), expected);
        assert_eq!(decode_event::<SetRaceCheckpoint>(&expected).unwrap(), ev);
    }

    #[test]
    fn display_game_text_golden_bytes() {
        let ev = DisplayGameText {
            style: 5,
            time: 1000,
            text: b"hi".to_vec(),
        };
        let expected = vec![
            0x05, 0x00, 0x00, 0x00, // style i32 = 5
            0xE8, 0x03, 0x00, 0x00, // time i32 = 1000
            0x02, 0x00, 0x00, 0x00, b'h', b'i', // text str32 = "hi"
        ];
        assert_eq!(encode_event(&ev), expected);
        assert_eq!(decode_event::<DisplayGameText>(&expected).unwrap(), ev);
    }
}
