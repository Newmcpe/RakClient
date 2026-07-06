//! RPC and sync packet identifiers (verified against the binary's RPC id table & sync sender).
//!
//! `TryFrom<u8>` / `From<Self> for u8` come from `num_enum`; the variant list (`Self::VARIANTS`,
//! the equivalent of Java's `Enum.values()`) comes from `strum::VariantArray`.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use strum::VariantArray;

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive, VariantArray)]
#[repr(u8)]
pub enum RpcId {
    ClientJoin = 25,
    Spawn = 52,
    /// Server→client dialog (login/registration/menus). Body:
    /// `[u16 dialogId][u8 style][str8 title][str8 button1][str8 button2][compressed body]`.
    ShowDialog = 61,
    /// Client→server reply to a `ShowDialog`: `[u16 dialogId][u8 button][u16 listItem][u8 len][text]`.
    DialogResponse = 62,
    /// Server→client coloured text line (SA-MP `SendClientMessage`); also how Arizona delivers
    /// most chat. Body: `[u32 colour LE][u32 len LE][text]` (text is cp1251).
    ClientMessage = 93,
    /// Player chat. Client→server send body `[u8 len][text]`; server→client broadcast body
    /// `[u16 playerId LE][u8 len][text]`.
    Chat = 101,
    /// `RPC_ScrSetSpawnInfo`: the server hands the client its spawn position; `Net_Spawn` copies it
    /// into the local sync position at spawn time.
    SetSpawnInfo = 68,
    /// `RPC_ScrTogglePlayerSpectating`: `1 → 0` drops the client out of spectate (the Arizona
    /// post-login spawn trigger).
    TogglePlayerSpectating = 124,
    RequestClass = 128,
    RequestSpawn = 129,
    ConnectionRejected = 130,
    ServerJoin = 137,
    ServerQuit = 138,
    InitGame = 139,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive, VariantArray)]
#[repr(u8)]
pub enum SyncPacketId {
    VehicleSync = 200,
    AimSync = 203,
    /// Client→server weapon inventory snapshot (`PACKET_WEAPONS_UPDATE`).
    WeaponsUpdate = 204,
    /// Client→server player stats (`PACKET_STATS_UPDATE`): `[i32 money][i32 drunk]`. The real client
    /// sends it every second while spawned (`NetGame_Process` @0x10005B10).
    StatsUpdate = 205,
    BulletSync = 206,
    PlayerSync = 207,
    MarkersSync = 208,
    UnoccupiedSync = 209,
    TrailerSync = 210,
    PassengerSync = 211,
    SpectatorSync = 212,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_known_ids() {
        assert_eq!(RpcId::try_from(25u8).unwrap(), RpcId::ClientJoin);
        assert_eq!(RpcId::try_from(139u8).unwrap(), RpcId::InitGame);
        assert_eq!(u8::from(RpcId::ClientJoin), 25);
    }

    #[test]
    fn rpc_unknown_id_errs() {
        assert!(RpcId::try_from(0u8).is_err());
        assert!(RpcId::try_from(255u8).is_err());
    }

    #[test]
    fn sync_known_ids() {
        assert_eq!(
            SyncPacketId::try_from(207u8).unwrap(),
            SyncPacketId::PlayerSync
        );
        assert_eq!(
            SyncPacketId::try_from(200u8).unwrap(),
            SyncPacketId::VehicleSync
        );
        assert_eq!(u8::from(SyncPacketId::PlayerSync), 207);
    }

    #[test]
    fn sync_unknown_id_errs() {
        assert!(SyncPacketId::try_from(199u8).is_err());
        assert!(SyncPacketId::try_from(213u8).is_err());
    }

    #[test]
    fn variants_list_like_java_values() {
        assert!(RpcId::VARIANTS.contains(&RpcId::Spawn));
        assert_eq!(SyncPacketId::VARIANTS.len(), 11);
    }
}
