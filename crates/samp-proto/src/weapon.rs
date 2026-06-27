//! SA-MP weapon inventory wire support: the per-slot snapshot a client streams via
//! `PACKET_WEAPONS_UPDATE` (204), plus the weapon→slot and weapon→state lookup tables. Pure
//! protocol knowledge — the decision of *when* to send and *what* the inventory holds is the
//! caller's (see `samp-client`).

use crate::BitStreamWriter;

/// Number of weapon slots a SA-MP client tracks (slot ids `0..=12`).
pub const WEAPON_SLOTS: usize = 13;

/// One weapon slot: the armed weapon id and its ammo count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WeaponSlot {
    pub weapon: u8,
    pub ammo: u16,
}

/// The inventory slot (`0..=12`) a weapon id occupies, or `None` for ids with no slot. Ported from
/// the 0.3.7 weapon-slot table.
pub fn weapon_slot(weapon: u8) -> Option<u8> {
    let slot = match weapon {
        0..=1 => 0,
        2..=9 => 1,
        10..=15 => 10,
        16..=18 => 8,
        22..=24 => 2,
        25..=27 => 3,
        28 | 29 | 32 => 4,
        30 | 31 => 5,
        33 | 34 => 6,
        35..=38 => 7,
        39 => 8,
        40 => 12,
        41..=43 => 9,
        44..=46 => 11,
        _ => return None,
    };
    Some(slot)
}

/// The weapon "state" byte the aim-sync packet reports for the armed weapon (`0` for ids that have
/// no state). Ported from the 0.3.7 weapon-state table.
pub fn weapon_state(weapon: u8) -> u8 {
    match weapon {
        16..=18 | 25 | 33..=36 | 39 => 1,
        22..=24 | 26..=32 | 37 | 38 | 41..=43 => 2,
        _ => 0,
    }
}

/// Encode the `PACKET_WEAPONS_UPDATE` (204) body: two `0xFFFF` words (unused player/vehicle ids)
/// followed by every slot as `[u8 slot][u8 weapon][u16 ammo]`. The id byte is prepended by the
/// transport.
pub fn encode_weapons_update(slots: &[WeaponSlot; WEAPON_SLOTS]) -> Vec<u8> {
    let mut w = BitStreamWriter::new();
    w.write_u16(0xFFFF);
    w.write_u16(0xFFFF);
    for (slot_id, slot) in slots.iter().enumerate() {
        w.write_u8(slot_id as u8);
        w.write_u8(slot.weapon);
        w.write_u16(slot.ammo);
    }
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weapon_slots_match_the_0_3_7_table() {
        assert_eq!(weapon_slot(0), Some(0));
        assert_eq!(weapon_slot(24), Some(2)); // tec9 -> slot 2 (machine pistols share with smg? no)
        assert_eq!(weapon_slot(31), Some(5)); // m4 -> slot 5
        assert_eq!(weapon_slot(34), Some(6)); // sniper -> slot 6
        assert_eq!(weapon_slot(39), Some(8)); // satchel detonator -> slot 8
        assert_eq!(weapon_slot(40), Some(12)); // detonator -> slot 12
        assert_eq!(weapon_slot(46), Some(11)); // parachute -> slot 11
        assert_eq!(weapon_slot(19), None);
        assert_eq!(weapon_slot(200), None);
    }

    #[test]
    fn weapon_states_match_the_0_3_7_table() {
        assert_eq!(weapon_state(16), 1); // grenade
        assert_eq!(weapon_state(25), 1); // shotgun
        assert_eq!(weapon_state(24), 2); // deagle
        assert_eq!(weapon_state(31), 2); // m4
        assert_eq!(weapon_state(0), 0);
        assert_eq!(weapon_state(255), 0);
    }

    #[test]
    fn weapons_update_golden_vector() {
        let mut slots = [WeaponSlot::default(); WEAPON_SLOTS];
        slots[2] = WeaponSlot {
            weapon: 24,
            ammo: 50,
        };
        let body = encode_weapons_update(&slots);
        // 2 + 2 header bytes, then 13 * (1 + 1 + 2) slot bytes.
        assert_eq!(body.len(), 4 + WEAPON_SLOTS * 4);
        assert_eq!(&body[0..4], &[0xFF, 0xFF, 0xFF, 0xFF]);
        // Slot 2 entry begins at offset 4 + 2*4 = 12: [slot=2][weapon=24][ammo=50 LE].
        assert_eq!(&body[12..16], &[2, 24, 50, 0]);
        // Slot 0 entry: [slot=0][weapon=0][ammo=0].
        assert_eq!(&body[4..8], &[0, 0, 0, 0]);
    }
}
