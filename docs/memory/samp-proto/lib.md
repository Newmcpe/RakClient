# samp-proto/lib.rs

## Module — provenance highlights
<anchor: module-provenance>

SA-MP 0.3.7 wire protocol — pure codecs, no I/O.

This crate is the **public contract** other crates compile against. The bit-packing and the
RPC/sync layouts are ports of the original 0.3.7 client (verified against the binary's
`BitStream_WriteBits`/`BitStream_ReadBits`, the `RPC_ClientJoin` handshake, `RPC_InitGame`,
the class/spawn request paths, and the on-foot branch of the in-game sync sender).

Provenance highlights (verified in the binary):
- RakNet bit order: bits are packed MSB-first within a byte; multi-byte integers are stored in
  little-endian byte order, then bit-packed.
- `ClientJoin`: `version = 4057`, `challenge_response = server_cookie ^ 0xFD9`.
- On-foot sync body is exactly 544 bits / 68 bytes.

---

## OutboundMsg::Packet
<anchor: outboundmsg-packet>

A raw application packet (`data[0]` = id). `reliability` is the RakNet wire value
(`0..=4` = Unreliable/UnreliableSequenced/Reliable/ReliableOrdered/ReliableSequenced) so
this crate stays raknet-free; the driver maps it to `raknet::Reliability`. `sendPacket()`
defaults to reliable-ordered (`3`) on channel `0` — the Arizona `220` path.

---

## CP1251_HIGH
<anchor: cp1251-high>

Unicode scalar values for Windows-1251 (cp1251) bytes `0x80..=0xFF`. SA-MP Russian servers
(Arizona) send chat/system text in cp1251, so chat bytes must be transcoded for display.
`0x98` is unassigned in cp1251 and maps to the replacement character. `0xC0..=0xFF` is the
contiguous `А..я` block and is computed rather than tabled.

---
