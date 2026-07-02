//! World and environment: time/weather/gravity, interiors, checkpoints, pickups, map icons,
//! gang zones, explosions, audio, and misc world-scoped effects.

use crate::events::{
    field_ty, from_value, incoming_event, read_field, samp_event, to_value, write_field,
};

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
    /// onSetWorldTime — RPC 94.
    SetWorldTime = 94, "onSetWorldTime" {
        hour: u8,
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
    /// onSetInterior — RPC 156.
    SetInterior = 156, "onSetInterior" {
        interior: u8,
    }
}

incoming_event! {
    /// onSetCheckpoint — RPC 107.
    SetCheckpoint = 107, "onSetCheckpoint" {
        position: vector3,
        radius: f32,
    }
}

incoming_event! {
    /// onDisableCheckpoint — RPC 37.
    DisableCheckpoint = 37, "onDisableCheckpoint" {}
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
    /// onDisableRaceCheckpoint — RPC 39.
    DisableRaceCheckpoint = 39, "onDisableRaceCheckpoint" {}
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
    /// onStopAudioStream — RPC 42.
    StopAudioStream = 42, "onStopAudioStream" {}
}

incoming_event! {
    /// onCreateExplosion — RPC 79.
    CreateExplosion = 79, "onCreateExplosion" {
        position: vector3,
        style: i32,
        radius: f32,
    }
}

incoming_event! {
    /// onCreatePickup — RPC 95.
    CreatePickup = 95, "onCreatePickup" {
        id: i32,
        model: i32,
        pickup_type: i32,
        position: vector3,
    }
}

incoming_event! {
    /// onDestroyPickup — RPC 63.
    DestroyPickup = 63, "onDestroyPickup" {
        id: i32,
    }
}

incoming_event! {
    /// onDestroyWeaponPickup — RPC 151.
    DestroyWeaponPickup = 151, "onDestroyWeaponPickup" {
        id: u8,
    }
}

incoming_event! {
    /// onSetMapIcon — RPC 56.
    SetMapIcon = 56, "onSetMapIcon" {
        icon_id: u8,
        position: vector3,
        kind: u8,
        color: i32,
        style: u8,
    }
}

incoming_event! {
    /// onRemoveMapIcon — RPC 144.
    RemoveMapIcon = 144, "onRemoveMapIcon" {
        icon_id: u8,
    }
}

incoming_event! {
    /// onUpdateGlobalTimer — RPC 60.
    UpdateGlobalTimer = 60, "onUpdateGlobalTimer" {
        time: i32,
    }
}

incoming_event! {
    /// onSetShopName — RPC 33.
    SetShopName = 33, "onSetShopName" {
        name: fixed32,
    }
}

incoming_event! {
    /// onRemove3DTextLabel — RPC 58.
    Remove3DTextLabel = 58, "onRemove3DTextLabel" {
        text_label_id: u16,
    }
}

incoming_event! {
    /// onCreateGangZone — RPC 108.
    CreateGangZone = 108, "onCreateGangZone" {
        zone_id: u16,
        square_start: vector2,
        square_end: vector2,
        color: i32,
    }
}

incoming_event! {
    /// onGangZoneFlash — RPC 121.
    GangZoneFlash = 121, "onGangZoneFlash" {
        zone_id: u16,
        color: i32,
    }
}

incoming_event! {
    /// onGangZoneStopFlash — RPC 85.
    GangZoneStopFlash = 85, "onGangZoneStopFlash" {
        zone_id: u16,
    }
}

incoming_event! {
    /// onGangZoneDestroy — RPC 120.
    GangZoneDestroy = 120, "onGangZoneDestroy" {
        zone_id: u16,
    }
}

incoming_event! {
    /// onPlayCrimeReport — RPC 112.
    PlayCrimeReport = 112, "onPlayCrimeReport" {
        suspect_id: u16,
        in_vehicle: bool32,
        vehicle_model: i32,
        vehicle_color: i32,
        crime: i32,
        coordinates: vector3,
    }
}

incoming_event! {
    /// onEnableStuntBonus — RPC 104.
    EnableStuntBonus = 104, "onEnableStuntBonus" {
        state: bool,
    }
}
