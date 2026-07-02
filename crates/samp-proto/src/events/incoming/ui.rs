//! On-screen UI: game text, menus, textdraws, and display toggles.

use crate::events::{
    field_ty, from_value, incoming_event, read_field, samp_event, to_value, write_field,
};

incoming_event! {
    /// onDisplayGameText — `RPC_ScrDisplayGameText` (73).
    DisplayGameText = 73, "onDisplayGameText" {
        style: i32,
        time: i32,
        text: str32,
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
    /// onToggleSelectTextDraw — RPC 83.
    ToggleSelectTextDraw = 83, "onToggleSelectTextDraw" {
        state: bool,
        hovercolor: i32,
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
    /// onTextDrawHide — RPC 135.
    TextDrawHide = 135, "onTextDrawHide" {
        text_draw_id: u16,
    }
}

incoming_event! {
    /// onToggleWidescreen — RPC 111.
    ToggleWidescreen = 111, "onToggleWidescreen" {
        enable: bool8,
    }
}
