//! Incoming (server→client) RPC events with fixed field layouts, ported from `INCOMING_RPCS` in
//! `samp/events.lua`.
//!
//! Content-dependent incoming RPCs (`onInitGame`, `onInitMenu`, `onCreateObject`,
//! `onSetObjectMaterial(Text)`, `onUpdateScoresAndPings`, vehicle stream-in) carry nested or
//! end-of-stream data and are hand-written elsewhere rather than via [`samp_event!`].

use super::{field_ty, from_value, incoming_event, read_field, samp_event, to_value, write_field};

incoming_event! {
    /// onPlayerJoin — `RPC_ScrServerJoin` (137).
    PlayerJoin = 137, "onPlayerJoin" {
        player_id: u16,
        color: i32,
        is_npc: bool8,
        nickname: str8,
    }
}

incoming_event! {
    /// onPlayerQuit — `RPC_ScrServerQuit` (138).
    PlayerQuit = 138, "onPlayerQuit" {
        player_id: u16,
        reason: u8,
    }
}

incoming_event! {
    /// onSetPlayerPos — `RPC_ScrSetPlayerPos` (12).
    SetPlayerPos = 12, "onSetPlayerPos" {
        position: vector3,
    }
}

incoming_event! {
    /// onSetPlayerPosFindZ — `RPC_ScrSetPlayerPosFindZ` (13).
    SetPlayerPosFindZ = 13, "onSetPlayerPosFindZ" {
        position: vector3,
    }
}

incoming_event! {
    /// onSetPlayerHealth — `RPC_ScrSetPlayerHealth` (14).
    SetPlayerHealth = 14, "onSetPlayerHealth" {
        health: f32,
    }
}

incoming_event! {
    /// onTogglePlayerControllable — `RPC_ScrTogglePlayerControllable` (15).
    TogglePlayerControllable = 15, "onTogglePlayerControllable" {
        controllable: bool8,
    }
}

incoming_event! {
    /// onPlaySound — `RPC_ScrPlaySound` (16).
    PlaySound = 16, "onPlaySound" {
        sound_id: i32,
        position: vector3,
    }
}

incoming_event! {
    /// onSetWorldBounds — `RPC_ScrSetPlayerWorldBounds` (17).
    SetWorldBounds = 17, "onSetWorldBounds" {
        max_x: f32,
        min_x: f32,
        max_y: f32,
        min_y: f32,
    }
}

incoming_event! {
    /// onGivePlayerMoney — `RPC_ScrGivePlayerMoney` (18).
    GivePlayerMoney = 18, "onGivePlayerMoney" {
        money: i32,
    }
}

incoming_event! {
    /// onSetPlayerFacingAngle — `RPC_ScrSetPlayerFacingAngle` (19).
    SetPlayerFacingAngle = 19, "onSetPlayerFacingAngle" {
        angle: f32,
    }
}

incoming_event! {
    /// onResetPlayerMoney — `RPC_ScrResetPlayerMoney` (20).
    ResetPlayerMoney = 20, "onResetPlayerMoney" {}
}

incoming_event! {
    /// onResetPlayerWeapons — `RPC_ScrResetPlayerWeapons` (21).
    ResetPlayerWeapons = 21, "onResetPlayerWeapons" {}
}

incoming_event! {
    /// onGivePlayerWeapon — `RPC_ScrGivePlayerWeapon` (22).
    GivePlayerWeapon = 22, "onGivePlayerWeapon" {
        weapon_id: i32,
        ammo: i32,
    }
}

incoming_event! {
    /// onSetPlayerTime — `RPC_ScrSetPlayerTime` (29).
    SetPlayerTime = 29, "onSetPlayerTime" {
        hour: u8,
        minute: u8,
    }
}

incoming_event! {
    /// onSetToggleClock — `RPC_ScrToggleClock` (30).
    SetToggleClock = 30, "onSetToggleClock" {
        state: bool8,
    }
}

incoming_event! {
    /// onSetPlayerSkillLevel — `RPC_ScrSetPlayerSkillLevel` (34).
    SetPlayerSkillLevel = 34, "onSetPlayerSkillLevel" {
        player_id: u16,
        skill: i32,
        level: u16,
    }
}

incoming_event! {
    /// onSetPlayerDrunk — `RPC_ScrSetPlayerDrunkLevel` (35).
    SetPlayerDrunk = 35, "onSetPlayerDrunk" {
        drunk_level: i32,
    }
}

incoming_event! {
    /// onSetRaceCheckpoint — `RPC_ScrSetRaceCheckpoint` (38).
    SetRaceCheckpoint = 38, "onSetRaceCheckpoint" {
        kind: u8,
        position: vector3,
        next_position: vector3,
        size: f32,
    }
}

incoming_event! {
    /// onPlayAudioStream — `RPC_ScrPlayAudioStream` (41).
    PlayAudioStream = 41, "onPlayAudioStream" {
        url: str8,
        position: vector3,
        radius: f32,
        use_position: bool8,
    }
}

incoming_event! {
    /// onSetPlayerArmour — `RPC_ScrSetPlayerArmour` (66).
    SetPlayerArmour = 66, "onSetPlayerArmour" {
        armour: f32,
    }
}

incoming_event! {
    /// onSetPlayerArmedWeapon — `RPC_ScrSetPlayerArmedWeapon` (67).
    SetPlayerArmedWeapon = 67, "onSetPlayerArmedWeapon" {
        weapon_id: i32,
    }
}

incoming_event! {
    /// onSetPlayerTeam — `RPC_ScrSetPlayerTeam` (69).
    SetPlayerTeam = 69, "onSetPlayerTeam" {
        player_id: u16,
        team_id: u8,
    }
}

incoming_event! {
    /// onPutPlayerInVehicle — `RPC_ScrPutPlayerInVehicle` (70).
    PutPlayerInVehicle = 70, "onPutPlayerInVehicle" {
        vehicle_id: u16,
        seat_id: u8,
    }
}

incoming_event! {
    /// onSetPlayerColor — `RPC_ScrSetPlayerColor` (72).
    SetPlayerColor = 72, "onSetPlayerColor" {
        player_id: u16,
        color: i32,
    }
}

incoming_event! {
    /// onDisplayGameText — `RPC_ScrDisplayGameText` (73).
    DisplayGameText = 73, "onDisplayGameText" {
        style: i32,
        time: i32,
        text: str32,
    }
}

incoming_event! {
    /// onSetPlayerSkin — `RPC_ScrSetPlayerSkin` (153). `player_id` is `int32` on the wire here
    /// (faithful to `events.lua`: `{playerId='int32'}`), unlike the `uint16` used by other RPCs.
    SetPlayerSkin = 153, "onSetPlayerSkin" {
        player_id: i32,
        skin_id: i32,
    }
}

incoming_event! {
    /// onSetWeather — `RPC_ScrSetWeather` (152).
    SetWeather = 152, "onSetWeather" {
        weather_id: u8,
    }
}

incoming_event! {
    /// onSetGravity — `RPC_ScrSetGravity` (146).
    SetGravity = 146, "onSetGravity" {
        gravity: f32,
    }
}

incoming_event! {
    /// onPlayerStreamIn — `RPC_ScrWorldPlayerAdd` (32).
    PlayerStreamIn = 32, "onPlayerStreamIn" {
        player_id: u16,
        team: u8,
        model: i32,
        position: vector3,
        rotation: f32,
        color: i32,
        fighting_style: u8,
    }
}

incoming_event! {
    /// onPlayerStreamOut — `RPC_ScrWorldPlayerRemove` (163).
    PlayerStreamOut = 163, "onPlayerStreamOut" {
        player_id: u16,
    }
}

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
