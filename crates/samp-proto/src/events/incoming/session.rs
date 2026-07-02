//! Connection lifecycle: join/quit, class selection, spawn authorisation, spectate toggles, and
//! the server's client-check probes.

use crate::events::{
    field_ty, from_value, incoming_event, read_field, samp_event, to_value, write_field,
};

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
    /// onConnectionRejected — RPC 130.
    ConnectionRejected = 130, "onConnectionRejected" {
        reason: u8,
    }
}

incoming_event! {
    /// onRequestSpawnResponse — RPC 129.
    RequestSpawnResponse = 129, "onRequestSpawnResponse" {
        response: bool8,
    }
}

incoming_event! {
    /// onSetSpawnInfo — RPC 68.
    SetSpawnInfo = 68, "onSetSpawnInfo" {
        team: u8,
        skin: i32,
        unused: u8,
        position: vector3,
        rotation: f32,
        weapons: int_array3,
        ammo: int_array3,
    }
}

incoming_event! {
    /// onForceClassSelection — RPC 74.
    ForceClassSelection = 74, "onForceClassSelection" {}
}

incoming_event! {
    /// onTogglePlayerSpectating — RPC 124.
    TogglePlayerSpectating = 124, "onTogglePlayerSpectating" {
        state: bool32,
    }
}

incoming_event! {
    /// onSpectatePlayer — RPC 126.
    SpectatePlayer = 126, "onSpectatePlayer" {
        player_id: u16,
        cam_type: u8,
    }
}

incoming_event! {
    /// onSpectateVehicle — RPC 127.
    SpectateVehicle = 127, "onSpectateVehicle" {
        vehicle_id: u16,
        cam_type: u8,
    }
}

incoming_event! {
    /// onGamemodeRestart — RPC 40.
    GamemodeRestart = 40, "onGamemodeRestart" {}
}

incoming_event! {
    /// onClientCheck — RPC 103.
    ClientCheck = 103, "onClientCheck" {
        request_type: u8,
        subject: i32,
        offset: u16,
        length: u16,
    }
}

incoming_event! {
    /// onServerStatisticsResponse — RPC 102.
    ServerStatisticsResponse = 102, "onServerStatisticsResponse" {}
}
