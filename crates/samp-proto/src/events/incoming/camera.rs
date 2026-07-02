//! Camera control: positioning, look-at, interpolation, and target notification.

use crate::events::{
    field_ty, from_value, incoming_event, read_field, samp_event, to_value, write_field,
};

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
    /// onSetCameraBehind — RPC 162.
    SetCameraBehind = 162, "onSetCameraBehind" {}
}

incoming_event! {
    /// onToggleCameraTargetNotifying — RPC 170.
    ToggleCameraTargetNotifying = 170, "onToggleCameraTargetNotifying" {
        enable: bool,
    }
}
