//! World objects: placement, movement, attachment, and the object-edit/select modes.

use crate::events::{
    field_ty, from_value, incoming_event, read_field, samp_event, to_value, write_field,
};

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
    /// onStopObject — RPC 122.
    StopObject = 122, "onStopObject" {
        object_id: u16,
    }
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
    /// onEnterSelectObject — RPC 27.
    EnterSelectObject = 27, "onEnterSelectObject" {}
}

incoming_event! {
    /// onSetPlayerObjectNoCameraCol — RPC 169.
    SetPlayerObjectNoCameraCol = 169, "onSetPlayerObjectNoCameraCol" {
        object_id: u16,
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
