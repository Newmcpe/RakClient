//! Incoming (server→client) RPC events, fixed layouts (part 2), ported from `INCOMING_RPCS` in
//! `samp/events.lua`.

use super::{field_ty, from_value, incoming_event, read_field, samp_event, to_value, write_field};

incoming_event! {
    /// onRequestSpawnResponse — RPC 129.
    RequestSpawnResponse = 129, "onRequestSpawnResponse" {
        response: bool8,
    }
}

incoming_event! {
    /// onSetShopName — RPC 33.
    SetShopName = 33, "onSetShopName" {
        name: fixed32,
    }
}

incoming_event! {
    /// onRemoveBuilding — RPC 43.
    RemoveBuilding = 43, "onRemoveBuilding" {
        model_id: i32,
        position: vector3,
        radius: f32,
    }
}

incoming_event! {
    /// onSetObjectPosition — RPC 45.
    SetObjectPosition = 45, "onSetObjectPosition" {
        object_id: u16,
        position: vector3,
    }
}

incoming_event! {
    /// onSetObjectRotation — RPC 46.
    SetObjectRotation = 46, "onSetObjectRotation" {
        object_id: u16,
        rotation: vector3,
    }
}

incoming_event! {
    /// onDestroyObject — RPC 47.
    DestroyObject = 47, "onDestroyObject" {
        object_id: u16,
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
    /// onRemoveVehicleComponent — RPC 57.
    RemoveVehicleComponent = 57, "onRemoveVehicleComponent" {
        vehicle_id: u16,
        component_id: u16,
    }
}

incoming_event! {
    /// onRemove3DTextLabel — RPC 58.
    Remove3DTextLabel = 58, "onRemove3DTextLabel" {
        text_label_id: u16,
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

incoming_event! {
    /// onUpdateGlobalTimer — RPC 60.
    UpdateGlobalTimer = 60, "onUpdateGlobalTimer" {
        time: i32,
    }
}

incoming_event! {
    /// onDestroyPickup — RPC 63.
    DestroyPickup = 63, "onDestroyPickup" {
        id: i32,
    }
}

incoming_event! {
    /// onLinkVehicleToInterior — RPC 65.
    LinkVehicleToInterior = 65, "onLinkVehicleToInterior" {
        vehicle_id: u16,
        interior_id: u8,
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
    /// onRemovePlayerFromVehicle — RPC 71.
    RemovePlayerFromVehicle = 71, "onRemovePlayerFromVehicle" {}
}

incoming_event! {
    /// onForceClassSelection — RPC 74.
    ForceClassSelection = 74, "onForceClassSelection" {}
}

incoming_event! {
    /// onAttachObjectToPlayer — RPC 75.
    AttachObjectToPlayer = 75, "onAttachObjectToPlayer" {
        object_id: u16,
        player_id: u16,
        offsets: vector3,
        rotation: vector3,
    }
}

incoming_event! {
    /// onShowMenu — RPC 77.
    ShowMenu = 77, "onShowMenu" {
        menu_id: u8,
    }
}

incoming_event! {
    /// onHideMenu — RPC 78.
    HideMenu = 78, "onHideMenu" {
        menu_id: u8,
    }
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
    /// onShowPlayerNameTag — RPC 80.
    ShowPlayerNameTag = 80, "onShowPlayerNameTag" {
        player_id: u16,
        show: bool8,
    }
}

incoming_event! {
    /// onAttachCameraToObject — RPC 81.
    AttachCameraToObject = 81, "onAttachCameraToObject" {
        object_id: u16,
    }
}

incoming_event! {
    /// onInterpolateCamera — RPC 82.
    InterpolateCamera = 82, "onInterpolateCamera" {
        set_pos: bool,
        from_pos: vector3,
        dest_pos: vector3,
        time: i32,
        mode: u8,
    }
}

incoming_event! {
    /// onDisableCheckpoint — RPC 37.
    DisableCheckpoint = 37, "onDisableCheckpoint" {}
}

incoming_event! {
    /// onDisableRaceCheckpoint — RPC 39.
    DisableRaceCheckpoint = 39, "onDisableRaceCheckpoint" {}
}

incoming_event! {
    /// onGamemodeRestart — RPC 40.
    GamemodeRestart = 40, "onGamemodeRestart" {}
}

incoming_event! {
    /// onStopAudioStream — RPC 42.
    StopAudioStream = 42, "onStopAudioStream" {}
}

incoming_event! {
    /// onSetWorldTime — RPC 94.
    SetWorldTime = 94, "onSetWorldTime" {
        hour: u8,
    }
}

incoming_event! {
    /// onSetInterior — RPC 156.
    SetInterior = 156, "onSetInterior" {
        interior: u8,
    }
}

incoming_event! {
    /// onSetCameraPosition — RPC 157.
    SetCameraPosition = 157, "onSetCameraPosition" {
        position: vector3,
    }
}

incoming_event! {
    /// onSetCameraLookAt — RPC 158.
    SetCameraLookAt = 158, "onSetCameraLookAt" {
        look_at_position: vector3,
        cut_type: u8,
    }
}

incoming_event! {
    /// onSetVehiclePosition — RPC 159.
    SetVehiclePosition = 159, "onSetVehiclePosition" {
        vehicle_id: u16,
        position: vector3,
    }
}

incoming_event! {
    /// onSetVehicleAngle — RPC 160.
    SetVehicleAngle = 160, "onSetVehicleAngle" {
        vehicle_id: u16,
        angle: f32,
    }
}

incoming_event! {
    /// onSetVehicleParams — RPC 161.
    SetVehicleParams = 161, "onSetVehicleParams" {
        vehicle_id: u16,
        objective: bool8,
        doors_locked: bool8,
    }
}

incoming_event! {
    /// onSetCameraBehind — RPC 162.
    SetCameraBehind = 162, "onSetCameraBehind" {}
}

incoming_event! {
    /// onConnectionRejected — RPC 130.
    ConnectionRejected = 130, "onConnectionRejected" {
        reason: u8,
    }
}

incoming_event! {
    /// onPlayerEnterVehicle — RPC 26.
    PlayerEnterVehicle = 26, "onPlayerEnterVehicle" {
        player_id: u16,
        vehicle_id: u16,
        passenger: bool8,
    }
}

incoming_event! {
    /// onPlayerExitVehicle — RPC 154.
    PlayerExitVehicle = 154, "onPlayerExitVehicle" {
        player_id: u16,
        vehicle_id: u16,
    }
}
