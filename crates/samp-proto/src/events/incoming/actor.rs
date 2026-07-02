//! Server-side actors (NPC props): lifecycle, positioning, and animation.

use crate::events::{
    field_ty, from_value, incoming_event, read_field, samp_event, to_value, write_field,
};

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
