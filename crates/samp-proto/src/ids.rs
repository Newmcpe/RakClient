//! RPC and sync packet identifiers (verified against the binary's RPC id table & sync sender).

use crate::{ProtoError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RpcId {
    ClientJoin = 25,
    Spawn = 52,
    /// Client→server reply to a `ShowDialog`: `[u16 dialogId][u8 button][u16 listItem][u8 len][text]`.
    DialogResponse = 62,
    /// Server→client coloured text line (SA-MP `SendClientMessage`); also how Arizona delivers
    /// most chat. Body: `[u32 colour LE][u32 len LE][text]` (text is cp1251).
    ClientMessage = 93,
    /// Player chat. Client→server send body `[u8 len][text]`; server→client broadcast body
    /// `[u16 playerId LE][u8 len][text]`.
    Chat = 101,
    RequestClass = 128,
    RequestSpawn = 129,
    ConnectionRejected = 130,
    ServerJoin = 137,
    ServerQuit = 138,
    InitGame = 139,
    /// Server→client dialog (login/registration/menus). Body:
    /// `[u16 dialogId][u8 style][str8 title][str8 button1][str8 button2][compressed body]`.
    ShowDialog = 61,
}

impl TryFrom<u8> for RpcId {
    type Error = ProtoError;

    /// ```
    /// use samp_proto::RpcId;
    /// assert_eq!(RpcId::try_from(25).unwrap(), RpcId::ClientJoin);
    /// assert!(RpcId::try_from(200).is_err());
    /// ```
    fn try_from(value: u8) -> Result<Self> {
        Ok(match value {
            25 => RpcId::ClientJoin,
            52 => RpcId::Spawn,
            128 => RpcId::RequestClass,
            129 => RpcId::RequestSpawn,
            130 => RpcId::ConnectionRejected,
            137 => RpcId::ServerJoin,
            138 => RpcId::ServerQuit,
            139 => RpcId::InitGame,
            other => return Err(ProtoError::UnknownRpc(other)),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SyncPacketId {
    VehicleSync = 200,
    AimSync = 203,
    BulletSync = 206,
    PlayerSync = 207,
    MarkersSync = 208,
    UnoccupiedSync = 209,
    TrailerSync = 210,
    PassengerSync = 211,
    SpectatorSync = 212,
}

impl TryFrom<u8> for SyncPacketId {
    type Error = ProtoError;

    /// ```
    /// use samp_proto::SyncPacketId;
    /// assert_eq!(SyncPacketId::try_from(207).unwrap(), SyncPacketId::PlayerSync);
    /// assert!(SyncPacketId::try_from(0).is_err());
    /// ```
    fn try_from(value: u8) -> Result<Self> {
        Ok(match value {
            200 => SyncPacketId::VehicleSync,
            203 => SyncPacketId::AimSync,
            206 => SyncPacketId::BulletSync,
            207 => SyncPacketId::PlayerSync,
            208 => SyncPacketId::MarkersSync,
            209 => SyncPacketId::UnoccupiedSync,
            210 => SyncPacketId::TrailerSync,
            211 => SyncPacketId::PassengerSync,
            212 => SyncPacketId::SpectatorSync,
            other => return Err(ProtoError::UnknownPacket(other)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_known_ids() {
        assert_eq!(RpcId::try_from(25), Ok(RpcId::ClientJoin));
        assert_eq!(RpcId::try_from(139), Ok(RpcId::InitGame));
        assert_eq!(RpcId::ClientJoin as u8, 25);
    }

    #[test]
    fn rpc_unknown_id_errs() {
        assert_eq!(RpcId::try_from(0), Err(ProtoError::UnknownRpc(0)));
        assert_eq!(RpcId::try_from(255), Err(ProtoError::UnknownRpc(255)));
    }

    #[test]
    fn sync_known_ids() {
        assert_eq!(SyncPacketId::try_from(207), Ok(SyncPacketId::PlayerSync));
        assert_eq!(SyncPacketId::try_from(200), Ok(SyncPacketId::VehicleSync));
        assert_eq!(SyncPacketId::PlayerSync as u8, 207);
    }

    #[test]
    fn sync_unknown_id_errs() {
        assert_eq!(
            SyncPacketId::try_from(199),
            Err(ProtoError::UnknownPacket(199))
        );
        assert_eq!(
            SyncPacketId::try_from(213),
            Err(ProtoError::UnknownPacket(213))
        );
    }
}
