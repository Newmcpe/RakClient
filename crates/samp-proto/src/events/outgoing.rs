//! Outgoing (client→server) RPC events, ported from `OUTCOMING_RPCS` in `samp/events.lua`.

use super::{field_ty, from_value, outgoing_event, read_field, samp_event, to_value, write_field};

outgoing_event! {
    /// SendEnterVehicle — RPC 26.
    SendEnterVehicle = 26, "onSendEnterVehicle" { vehicle_id: u16, passenger: bool8 }
}

outgoing_event! {
    /// SendClickPlayer — RPC 23.
    SendClickPlayer = 23, "onSendClickPlayer" { player_id: u16, source: u8 }
}

outgoing_event! {
    /// SendEnterEditObject — RPC 27.
    SendEnterEditObject = 27, "onSendEnterEditObject" { kind: i32, object_id: u16, model: i32, position: vector3 }
}

outgoing_event! {
    /// SendCommand — RPC 50.
    SendCommand = 50, "onSendCommand" { command: str32 }
}

outgoing_event! {
    /// SendSpawn — RPC 52.
    SendSpawn = 52, "onSendSpawn" {}
}

outgoing_event! {
    /// SendDeathNotification — RPC 53.
    SendDeathNotification = 53, "onSendDeathNotification" { reason: u8, killer_id: u16 }
}

outgoing_event! {
    /// SendDialogResponse — RPC 62.
    SendDialogResponse = 62, "onSendDialogResponse" { dialog_id: u16, button: u8, listbox_id: u16, input: str8 }
}

outgoing_event! {
    /// SendClickTextDraw — RPC 83.
    SendClickTextDraw = 83, "onSendClickTextDraw" { textdraw_id: u16 }
}

outgoing_event! {
    /// SendVehicleTuningNotification — RPC 96.
    SendVehicleTuningNotification = 96, "onSendVehicleTuningNotification" { vehicle_id: i32, param1: i32, param2: i32, event: i32 }
}

outgoing_event! {
    /// SendChat — RPC 101.
    SendChat = 101, "onSendChat" { message: str8 }
}

outgoing_event! {
    /// SendClientCheckResponse — RPC 103.
    SendClientCheckResponse = 103, "onSendClientCheckResponse" { request_type: u8, result1: i32, result2: u8 }
}

outgoing_event! {
    /// SendVehicleDamaged — RPC 106.
    SendVehicleDamaged = 106, "onSendVehicleDamaged" { vehicle_id: u16, panel_dmg: i32, door_dmg: i32, lights: u8, tires: u8 }
}

outgoing_event! {
    /// SendEditAttachedObject — RPC 116.
    SendEditAttachedObject = 116, "onSendEditAttachedObject" { response: i32, index: i32, model: i32, bone: i32, position: vector3, rotation: vector3, scale: vector3, color1: i32, color2: i32 }
}

outgoing_event! {
    /// SendEditObject — RPC 117.
    SendEditObject = 117, "onSendEditObject" { player_object: bool, object_id: u16, response: i32, position: vector3, rotation: vector3 }
}

outgoing_event! {
    /// SendInteriorChangeNotification — RPC 118.
    SendInteriorChangeNotification = 118, "onSendInteriorChangeNotification" { interior: u8 }
}

outgoing_event! {
    /// SendMapMarker — RPC 119.
    SendMapMarker = 119, "onSendMapMarker" { position: vector3 }
}

outgoing_event! {
    /// SendRequestClass — RPC 128.
    SendRequestClass = 128, "onSendRequestClass" { class_id: i32 }
}

outgoing_event! {
    /// SendRequestSpawn — RPC 129.
    SendRequestSpawn = 129, "onSendRequestSpawn" {}
}

outgoing_event! {
    /// SendPickedUpPickup — RPC 131.
    SendPickedUpPickup = 131, "onSendPickedUpPickup" { pickup_id: i32 }
}

outgoing_event! {
    /// SendMenuSelect — RPC 132.
    SendMenuSelect = 132, "onSendMenuSelect" { row: u8 }
}

outgoing_event! {
    /// SendVehicleDestroyed — RPC 136.
    SendVehicleDestroyed = 136, "onSendVehicleDestroyed" { vehicle_id: u16 }
}

outgoing_event! {
    /// SendQuitMenu — RPC 140.
    SendQuitMenu = 140, "onSendQuitMenu" {}
}

outgoing_event! {
    /// SendExitVehicle — RPC 154.
    SendExitVehicle = 154, "onSendExitVehicle" { vehicle_id: u16 }
}

outgoing_event! {
    /// SendUpdateScoresAndPings — RPC 155.
    SendUpdateScoresAndPings = 155, "onSendUpdateScoresAndPings" {}
}

outgoing_event! {
    /// SendMoneyIncreaseNotification — RPC 31.
    SendMoneyIncreaseNotification = 31, "onSendMoneyIncreaseNotification" { amount: i32, increase_type: i32 }
}

outgoing_event! {
    /// SendNpcJoin — RPC 54.
    SendNpcJoin = 54, "onSendNPCJoin" { version: i32, r#mod: u8, nickname: str8, challenge_response: i32 }
}

outgoing_event! {
    /// SendServerStatisticsRequest — RPC 102.
    SendServerStatisticsRequest = 102, "onSendServerStatisticsRequest" {}
}

outgoing_event! {
    /// SendPickedUpWeapon — RPC 97.
    SendPickedUpWeapon = 97, "onSendPickedUpWeapon" { id: u16 }
}

outgoing_event! {
    /// SendCameraTargetUpdate — RPC 168.
    SendCameraTargetUpdate = 168, "onSendCameraTargetUpdate" { object_id: u16, vehicle_id: u16, player_id: u16, actor_id: u16 }
}

outgoing_event! {
    /// SendGiveActorDamage — RPC 177.
    SendGiveActorDamage = 177, "onSendGiveActorDamage" { unused: bool, actor_id: u16, damage: f32, weapon: i32, bodypart: i32 }
}
