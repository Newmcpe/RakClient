//! Vehicle state: occupancy, position/velocity, damage, tuning, trailers, and streaming.

use crate::events::{
    field_ty, from_value, incoming_event, read_field, samp_event, to_value, write_field,
};

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

incoming_event! {
    /// onPutPlayerInVehicle — `RPC_ScrPutPlayerInVehicle` (70).
    PutPlayerInVehicle = 70, "onPutPlayerInVehicle" {
        vehicle_id: u16,
        seat_id: u8,
    }
}

incoming_event! {
    /// onRemovePlayerFromVehicle — RPC 71.
    RemovePlayerFromVehicle = 71, "onRemovePlayerFromVehicle" {}
}

incoming_event! {
    /// onLinkVehicleToInterior — RPC 65.
    LinkVehicleToInterior = 65, "onLinkVehicleToInterior" {
        vehicle_id: u16,
        interior_id: u8,
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
    /// onSetVehicleVelocity — RPC 91.
    SetVehicleVelocity = 91, "onSetVehicleVelocity" {
        turn: bool8,
        velocity: vector3,
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
    /// onSetVehicleHealth — RPC 147.
    SetVehicleHealth = 147, "onSetVehicleHealth" {
        vehicle_id: u16,
        health: f32,
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
    /// onVehicleStreamOut — RPC 165.
    VehicleStreamOut = 165, "onVehicleStreamOut" {
        vehicle_id: u16,
    }
}

incoming_event! {
    /// onDisableVehicleCollisions — RPC 167.
    DisableVehicleCollisions = 167, "onDisableVehicleCollisions" {
        disable: bool,
    }
}
