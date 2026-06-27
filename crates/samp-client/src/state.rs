//! The high-level local-player model, shared between the driver (authoritative sync) and the script
//! engine's `getBot*`/`setBot*` bindings.
//!
//! [`LocalPlayer`] composes typed sub-states — on-foot, in-vehicle, aim/camera and weapon inventory
//! — plus identity/world fields and the driver-control flags. The driver builds outgoing sync from
//! it and folds incoming RPCs back into it; native features (e.g. client emulation) read and write the
//! same model. Wire (de)serialisation stays in `samp-proto`; this is the in-memory view above it.

use std::cell::RefCell;
use std::net::SocketAddr;
use std::rc::Rc;

use samp_proto::{weapon_slot, weapon_state, Quaternion, Vector3, WeaponSlot, WEAPON_SLOTS};

/// On-foot movement/pose state — the fields the 0.3.7 on-foot sync (`OnFootSync`) carries.
#[derive(Debug, Clone, Copy, Default)]
pub struct OnFootData {
    pub keys: u16,
    pub position: Vector3,
    pub quaternion: Quaternion,
    pub health: u8,
    pub armour: u8,
    pub weapon: u8,
    pub special_action: u8,
}

/// In-vehicle state, present only while the bot occupies a vehicle.
#[derive(Debug, Clone, Copy, Default)]
pub struct InVehicleData {
    pub id: u16,
    pub seat: u8,
    pub position: Vector3,
    pub quaternion: Quaternion,
    pub health: u16,
    pub keys: u16,
    pub current_weapon: u8,
}

/// Aim/camera state, the high-level (unpacked) view of `AimSyncData`.
#[derive(Debug, Clone, Copy, Default)]
pub struct AimData {
    pub cam_mode: u8,
    pub cam_pos: Vector3,
    pub cam_front: Vector3,
    /// Camera external zoom (6 bits on the wire).
    pub ext_zoom: u8,
    /// Weapon state (2 bits on the wire).
    pub weapon_state: u8,
}

/// The bot's weapon inventory: the armed weapon plus the 13 SA-MP weapon slots. Mutated by the
/// give/set-ammo/set-armed/reset weapon RPCs; streamed to the server via `PACKET_WEAPONS_UPDATE`.
#[derive(Debug, Clone, Copy)]
pub struct WeaponInventory {
    pub current: u8,
    pub slots: [WeaponSlot; WEAPON_SLOTS],
}

impl Default for WeaponInventory {
    fn default() -> Self {
        Self {
            current: 0,
            slots: [WeaponSlot::default(); WEAPON_SLOTS],
        }
    }
}

impl WeaponInventory {
    /// `GivePlayerWeapon`: add a weapon (and ammo) to its slot and arm it. Returns whether the
    /// weapon mapped to a slot (and so the inventory changed).
    pub fn give(&mut self, weapon: u8, ammo: u16) -> bool {
        let Some(slot) = weapon_slot(weapon) else {
            return false;
        };
        self.update_slot(slot as usize, weapon, ammo);
        self.current = weapon;
        true
    }

    /// `SetWeaponAmmo`: replace the ammo in a weapon's slot (without arming it). Returns whether the
    /// weapon mapped to a slot.
    pub fn set_ammo(&mut self, weapon: u8, ammo: u16) -> bool {
        let Some(slot) = weapon_slot(weapon) else {
            return false;
        };
        // Clear the slot's weapon first so `update_slot` treats this as a fresh set, not an add.
        self.slots[slot as usize].weapon = 0;
        self.update_slot(slot as usize, weapon, ammo);
        true
    }

    /// `SetPlayerArmedWeapon`: change which weapon is currently held.
    pub fn set_armed(&mut self, weapon: u8) {
        self.current = weapon;
    }

    /// `ResetPlayerWeapons`: drop everything.
    pub fn reset(&mut self) {
        self.current = 0;
        self.slots = [WeaponSlot::default(); WEAPON_SLOTS];
    }

    /// The weapon-state byte the aim sync should report for the armed weapon.
    pub fn current_state(&self) -> u8 {
        weapon_state(self.current)
    }

    /// Same-weapon gives accumulate ammo; a different weapon replaces the slot's ammo.
    fn update_slot(&mut self, slot: usize, weapon: u8, ammo: u16) {
        let s = &mut self.slots[slot];
        s.ammo = if s.weapon != weapon {
            ammo
        } else {
            s.ammo.saturating_add(ammo)
        };
        s.weapon = weapon;
    }
}

/// The local bot's mutable state, shared between driver and scripts.
#[derive(Debug, Clone)]
pub struct LocalPlayer {
    pub nick: String,
    pub server_addr: SocketAddr,
    pub on_foot: OnFootData,
    pub vehicle: Option<InVehicleData>,
    pub aim: AimData,
    pub weapons: WeaponInventory,
    pub money: i32,
    pub interior: u8,
    pub score: i32,
    pub camera_pos: Vector3,
    /// Set by `updateSync()`; the driver sends a sync immediately and clears it.
    pub force_sync: bool,
    /// Set by `reconnect(ms)`; the driver schedules a reconnect after this delay and clears it.
    pub reconnect_in_ms: Option<u64>,
}

/// [`LocalPlayer`] shared between the (non-`Send`) driver and script engine on the client thread.
pub type SharedLocalPlayer = Rc<RefCell<LocalPlayer>>;

impl LocalPlayer {
    pub fn new(nick: String, server_addr: SocketAddr) -> Self {
        Self {
            nick,
            server_addr,
            on_foot: OnFootData::default(),
            vehicle: None,
            aim: AimData::default(),
            weapons: WeaponInventory::default(),
            money: 0,
            interior: 0,
            score: 0,
            camera_pos: Vector3::default(),
            force_sync: false,
            reconnect_in_ms: None,
        }
    }

    /// Build a shared handle seeded from the connection config.
    pub fn shared(nick: String, server_addr: SocketAddr) -> SharedLocalPlayer {
        Rc::new(RefCell::new(Self::new(nick, server_addr)))
    }

    /// Whether the bot currently occupies a vehicle.
    pub fn in_vehicle(&self) -> bool {
        self.vehicle.is_some()
    }

    /// The current vehicle id, or `0` when on foot (the SA-MP convention `getBotVehicle` reports).
    pub fn vehicle_id(&self) -> u16 {
        self.vehicle.map(|v| v.id).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weapon_inventory_give_arms_and_accumulates() {
        let mut inv = WeaponInventory::default();
        assert!(inv.give(24, 50)); // deagle -> slot 2
        assert_eq!(inv.current, 24);
        assert_eq!(inv.slots[2].weapon, 24);
        assert_eq!(inv.slots[2].ammo, 50);
        // Same weapon again accumulates ammo.
        assert!(inv.give(24, 10));
        assert_eq!(inv.slots[2].ammo, 60);
        // An id with no slot is rejected.
        assert!(!inv.give(19, 100));
    }

    #[test]
    fn weapon_inventory_set_ammo_replaces() {
        let mut inv = WeaponInventory::default();
        inv.give(24, 50);
        assert!(inv.set_ammo(24, 7));
        assert_eq!(inv.slots[2].ammo, 7);
        assert_eq!(inv.slots[2].weapon, 24);
    }

    #[test]
    fn weapon_inventory_reset_clears() {
        let mut inv = WeaponInventory::default();
        inv.give(31, 200);
        inv.set_armed(31);
        inv.reset();
        assert_eq!(inv.current, 0);
        assert!(inv.slots.iter().all(|s| s.weapon == 0 && s.ammo == 0));
    }
}
