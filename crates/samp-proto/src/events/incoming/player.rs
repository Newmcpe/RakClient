//! Player state: position, health/armour, money, weapons, appearance, animation, streaming, and
//! death notifications.

use crate::events::{
    field_ty, from_value, incoming_event, read_field, samp_event, to_value, write_field,
};

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
    /// onSetPlayerDrunkVisuals — RPC 92.
    SetPlayerDrunkVisuals = 92, "onSetPlayerDrunkVisuals" {
        level: i32,
    }
}

incoming_event! {
    /// onSetPlayerDrunkHandling — RPC 150.
    SetPlayerDrunkHandling = 150, "onSetPlayerDrunkHandling" {
        level: i32,
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
    /// onSetWeaponAmmo — RPC 145.
    SetWeaponAmmo = 145, "onSetWeaponAmmo" {
        weapon_id: u8,
        ammo: u16,
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
    /// onSetPlayerColor — `RPC_ScrSetPlayerColor` (72).
    SetPlayerColor = 72, "onSetPlayerColor" {
        player_id: u16,
        color: i32,
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

incoming_event! {
    /// onSetPlayerVelocity — RPC 90.
    SetPlayerVelocity = 90, "onSetPlayerVelocity" {
        velocity: vector3,
    }
}

incoming_event! {
    /// onSetPlayerSpecialAction — RPC 88.
    SetPlayerSpecialAction = 88, "onSetPlayerSpecialAction" {
        action_id: u8,
    }
}

incoming_event! {
    /// onSetPlayerFightingStyle — RPC 89.
    SetPlayerFightingStyle = 89, "onSetPlayerFightingStyle" {
        player_id: u16,
        style_id: u8,
    }
}

incoming_event! {
    /// onApplyPlayerAnimation — RPC 86.
    ApplyPlayerAnimation = 86, "onApplyPlayerAnimation" {
        player_id: u16,
        anim_lib: str8,
        anim_name: str8,
        frame_delta: f32,
        looping: bool,
        lock_x: bool,
        lock_y: bool,
        freeze: bool,
        time: i32,
    }
}

incoming_event! {
    /// onClearPlayerAnimation — RPC 87.
    ClearPlayerAnimation = 87, "onClearPlayerAnimation" {
        player_id: u16,
    }
}

incoming_event! {
    /// onSetPlayerWantedLevel — RPC 133.
    SetPlayerWantedLevel = 133, "onSetPlayerWantedLevel" {
        wanted_level: u8,
    }
}

incoming_event! {
    /// onPlayerDeath — RPC 166.
    PlayerDeath = 166, "onPlayerDeath" {
        player_id: u16,
    }
}

incoming_event! {
    /// onPlayerDeathNotification — RPC 55.
    PlayerDeathNotification = 55, "onPlayerDeathNotification" {
        killer_id: u16,
        killed_id: u16,
        reason: u8,
    }
}

incoming_event! {
    /// onShowPlayerNameTag — RPC 80.
    ShowPlayerNameTag = 80, "onShowPlayerNameTag" {
        player_id: u16,
        show: bool8,
    }
}

incoming_event! {
    /// onPlayerChatBubble — RPC 59.
    PlayerChatBubble = 59, "onPlayerChatBubble" {
        player_id: u16,
        color: i32,
        distance: f32,
        duration: i32,
        message: str8,
    }
}
