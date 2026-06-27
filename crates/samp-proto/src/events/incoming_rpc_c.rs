//! Incoming (server→client) RPC events, fixed layouts (part 3), ported from `INCOMING_RPCS` in
//! `samp/events.lua`.

use super::{field_ty, from_value, incoming_event, read_field, samp_event, to_value, write_field};

incoming_event! {
    /// onToggleSelectTextDraw — RPC 83.
    ToggleSelectTextDraw = 83, "onToggleSelectTextDraw" {
        state: bool,
        hovercolor: i32,
    }
}

incoming_event! {
    /// onGangZoneStopFlash — RPC 85.
    GangZoneStopFlash = 85, "onGangZoneStopFlash" {
        zone_id: u16,
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
    /// onSetPlayerVelocity — RPC 90.
    SetPlayerVelocity = 90, "onSetPlayerVelocity" {
        velocity: vector3,
    }
}

incoming_event! {
    /// onSetVehicleVelocity — RPC 91.
    SetVehicleVelocity = 91, "onSetVehicleVelocity" {
        turn: bool8,
        velocity: vector3,
    }
}

incoming_event! {
    /// onSetPlayerDrunkVisuals — RPC 92.
    SetPlayerDrunkVisuals = 92, "onSetPlayerDrunkVisuals" {
        level: i32,
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
    /// onVehicleTuningNotification — RPC 96.
    VehicleTuningNotification = 96, "onVehicleTuningNotification" {
        player_id: u16,
        event: i32,
        vehicle_id: i32,
        param1: i32,
        param2: i32,
    }
}

incoming_event! {
    /// onSetVehicleTires — RPC 98.
    SetVehicleTires = 98, "onSetVehicleTires" {
        vehicle_id: u16,
        tires: u8,
    }
}

incoming_event! {
    /// onMoveObject — RPC 99.
    MoveObject = 99, "onMoveObject" {
        object_id: u16,
        from_pos: vector3,
        dest_pos: vector3,
        speed: f32,
        rotation: vector3,
    }
}

incoming_event! {
    /// onServerStatisticsResponse — RPC 102.
    ServerStatisticsResponse = 102, "onServerStatisticsResponse" {}
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
    /// onEnableStuntBonus — RPC 104.
    EnableStuntBonus = 104, "onEnableStuntBonus" {
        state: bool,
    }
}

incoming_event! {
    /// onTextDrawSetString — RPC 105.
    TextDrawSetString = 105, "onTextDrawSetString" {
        id: u16,
        text: str16,
    }
}

incoming_event! {
    /// onVehicleDamageStatusUpdate — RPC 106.
    VehicleDamageStatusUpdate = 106, "onVehicleDamageStatusUpdate" {
        vehicle_id: u16,
        panel_dmg: i32,
        door_dmg: i32,
        lights: u8,
        tires: u8,
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
    /// onCreateGangZone — RPC 108.
    CreateGangZone = 108, "onCreateGangZone" {
        zone_id: u16,
        square_start: vector2,
        square_end: vector2,
        color: i32,
    }
}

incoming_event! {
    /// onToggleWidescreen — RPC 111.
    ToggleWidescreen = 111, "onToggleWidescreen" {
        enable: bool8,
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
    /// onEditAttachedObject — RPC 116.
    EditAttachedObject = 116, "onEditAttachedObject" {
        index: i32,
    }
}

incoming_event! {
    /// onEnterEditObject — RPC 117.
    EnterEditObject = 117, "onEnterEditObject" {
        player_object: bool,
        object_id: u16,
    }
}

incoming_event! {
    /// onGangZoneDestroy — RPC 120.
    GangZoneDestroy = 120, "onGangZoneDestroy" {
        zone_id: u16,
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
    /// onStopObject — RPC 122.
    StopObject = 122, "onStopObject" {
        object_id: u16,
    }
}

incoming_event! {
    /// onSetVehicleNumberPlate — RPC 123.
    SetVehicleNumberPlate = 123, "onSetVehicleNumberPlate" {
        vehicle_id: u16,
        text: str8,
    }
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
    /// onSetPlayerWantedLevel — RPC 133.
    SetPlayerWantedLevel = 133, "onSetPlayerWantedLevel" {
        wanted_level: u8,
    }
}

incoming_event! {
    /// onTextDrawHide — RPC 135.
    TextDrawHide = 135, "onTextDrawHide" {
        text_draw_id: u16,
    }
}

incoming_event! {
    /// onRemoveMapIcon — RPC 144.
    RemoveMapIcon = 144, "onRemoveMapIcon" {
        icon_id: u8,
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
    /// onSetVehicleHealth — RPC 147.
    SetVehicleHealth = 147, "onSetVehicleHealth" {
        vehicle_id: u16,
        health: f32,
    }
}

incoming_event! {
    /// onAttachTrailerToVehicle — RPC 148.
    AttachTrailerToVehicle = 148, "onAttachTrailerToVehicle" {
        trailer_id: u16,
        vehicle_id: u16,
    }
}

incoming_event! {
    /// onDetachTrailerFromVehicle — RPC 149.
    DetachTrailerFromVehicle = 149, "onDetachTrailerFromVehicle" {
        vehicle_id: u16,
    }
}

incoming_event! {
    /// onSetPlayerDrunkHandling — RPC 150.
    SetPlayerDrunkHandling = 150, "onSetPlayerDrunkHandling" {
        level: i32,
    }
}

incoming_event! {
    /// onDestroyWeaponPickup — RPC 151.
    DestroyWeaponPickup = 151, "onDestroyWeaponPickup" {
        id: u8,
    }
}

incoming_event! {
    /// onVehicleStreamOut — RPC 165.
    VehicleStreamOut = 165, "onVehicleStreamOut" {
        vehicle_id: u16,
    }
}

incoming_event! {
    /// onPlayerDeath — RPC 166.
    PlayerDeath = 166, "onPlayerDeath" {
        player_id: u16,
    }
}

incoming_event! {
    /// onDisableVehicleCollisions — RPC 167.
    DisableVehicleCollisions = 167, "onDisableVehicleCollisions" {
        disable: bool,
    }
}

incoming_event! {
    /// onSetPlayerObjectNoCameraCol — RPC 169.
    SetPlayerObjectNoCameraCol = 169, "onSetPlayerObjectNoCameraCol" {
        object_id: u16,
    }
}

incoming_event! {
    /// onToggleCameraTargetNotifying — RPC 170.
    ToggleCameraTargetNotifying = 170, "onToggleCameraTargetNotifying" {
        enable: bool,
    }
}

incoming_event! {
    /// onCreateActor — RPC 171.
    CreateActor = 171, "onCreateActor" {
        actor_id: u16,
        skin_id: i32,
        position: vector3,
        rotation: f32,
        health: f32,
    }
}

incoming_event! {
    /// onDestroyActor — RPC 172.
    DestroyActor = 172, "onDestroyActor" {
        actor_id: u16,
    }
}

incoming_event! {
    /// onApplyActorAnimation — RPC 173.
    ApplyActorAnimation = 173, "onApplyActorAnimation" {
        actor_id: u16,
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
    /// onClearActorAnimation — RPC 174.
    ClearActorAnimation = 174, "onClearActorAnimation" {
        actor_id: u16,
    }
}

incoming_event! {
    /// onSetActorFacingAngle — RPC 175.
    SetActorFacingAngle = 175, "onSetActorFacingAngle" {
        actor_id: u16,
        angle: f32,
    }
}

incoming_event! {
    /// onSetActorPos — RPC 176.
    SetActorPos = 176, "onSetActorPos" {
        actor_id: u16,
        position: vector3,
    }
}

incoming_event! {
    /// onSetActorHealth — RPC 178.
    SetActorHealth = 178, "onSetActorHealth" {
        actor_id: u16,
        health: f32,
    }
}

incoming_event! {
    /// onEnterSelectObject — RPC 27.
    EnterSelectObject = 27, "onEnterSelectObject" {}
}
