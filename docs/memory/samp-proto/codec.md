# samp-proto/codec.rs

## ClientJoin — duplicate challenge response
<anchor: clientjoin-duplicate-challenge>

When `duplicate_challenge_response` is set, `ClientJoin::encode` appends a trailing copy of
`challenge_response` as a 7th field — the Arizona 0.3.7-R3 client variant (verified in
`samp.dll:0x1000AA20`, where the join sender writes `challengeResponse` twice). Vanilla SA-MP
reads only the first six fields and ignores the trailing bytes, so this is safe to leave on; set
it `false` for a strict-vanilla join.

---

## ClientJoin::encode — field layout
<anchor: clientjoin-encode>

Build the `RPC_ClientJoin` body. Field order/sizes verified against
`Net_OnConnAccepted_SendClientJoin` (0x4572C0):
`version:u32, modded:u8, nick:str8, challenge_response:u32, auth:str8, client_version:str8`.
The Arizona variant appends a duplicated `challenge_response:u32` as a 7th field.

---

## RPC_InitGame — bit layout
<anchor: init-game-layout>

`RPC_InitGame` (139) bit layout (RPC_InitGame @0x458F90). The original handler consumes many
server-settings fields the client mostly discards; we replicate the exact cursor advance and
extract only the two fields this crate models.

---

## StatsUpdate — send schedule
<anchor: stats-update>

Outgoing `PACKET_STATS_UPDATE` (205): `[i32 money][i32 drunk]` (8 bytes). The real client sends
this every second while spawned (`NetGame_Process` @0x10005B10 writes the id then money and the
drunk level, each as a 32-bit little-endian int, `Send` UNRELIABLE on channel 0). The id byte is
prepended by the transport.

---

## SpectatorSync — pre-login spectate
<anchor: spectator-sync>

Outgoing spectator sync body (`ID_SPECTATOR_SYNC` = 212): `lrAnalog:u16, udAnalog:u16, keys:u16,
position:3xf32` (18 bytes). The real Arizona client spectates — sending this — while it answers the
login dialog, and only spawns afterwards; sending on-foot sync before login earns "ОШИБКА 7721".
The id byte is prepended by the transport.

---

## OnFootSync::encode — byte layout
<anchor: onfootsync-encode>

Encode the 544-bit on-foot sync body. Layout (byte offsets) ported from Net_SendInGameSync's
on-foot branch:
`lrAnalog:u16@0, udAnalog:u16@2, keys:u16@4, position:3xf32@6, quaternion:4xf32@18,
health:u8@34, armour:u8@35, weapon:u8@36, special_action:u8@37, moveSpeed:3xf32@38,
surfOffset:3xf32@50, surfVehicle:u16@62, animIndex:u16@64, animFlags:u16@66`.

The id byte is prepended by the caller/transport.

---

## OnFootSync::encode — quaternion order
<anchor: onfootsync-quat-order>

Quaternion on the wire is (w, x, y, z) — w FIRST. Verified against samp.dll's on-foot sender
(`CLocalPlayer_SendOnFootSync` @ 0x10004D40: the matrix→quat result is stored w-first at
body+0x12, with w ≥ 0). We previously wrote (x, y, z, w); the server then read our w as x etc.,
so any non-identity facing quat decoded as a ped tilted ~30° off vertical — the literal fly
pose — which is exactly what tripped Arizona's server-side "Fly Hack - Пешком(7/1)" on walkTo.
(The identity quat is symmetric enough to survive either order, which is why fly.to was safe.)

---

## ArizonaSync221 — custom on-foot position report
<anchor: arizona-sync-221>

Arizona's custom on-foot position report (`packet 221`, sub-id `53`) — a 28-byte, byte-aligned
sync the Arizona client streams alongside stock on-foot sync (207). The server anti-cheat kicks a
client that never sends it ("Игнорирование функции(52 / 1)", where 52 is the inbound 221 sub-id).
Layout reversed from live capture, all little-endian:
`[u8 221][u8 53][u8 0][u16 entity_id][f32 x][f32 y][f32 z][u32 timestamp_ms][4B velocity]
[u16 heading][u8 0x80]`. `timestamp_ms` must strictly increase (the server's replay/stall guard);
`velocity`/`heading` carry the rest values (`Self::REST_VELOCITY`/`Self::REST_HEADING`) when
the player is stationary.

---

## ShowDialog::body — Huffman-encoded body
<anchor: showdialog-body>

Decode the dialog BODY (the info text / list rows) that `Self::decode` skips — it follows the
head fields and is SA-MP Huffman-encoded (StringCompressor), same as 3D-text-label text. For
list/tablist dialogs this is the selectable options, `\n`-separated. Returns raw cp1251 bytes
(empty if the head is malformed or there is no body). Kept off the hot `decode` path so the
structural response path stays allocation-light.

The head is byte-aligned: `[u16 dialogId][u8 style][u8 tLen][title][u8 b1Len][b1][u8 b2Len]
[b2]`. Walk it by byte offset to find where the body starts, then Huffman-decode the rest
(the same StringCompressor `decode_string` that `readEncoded` uses).

---

## parse_connect — CONNECTION_REQUEST_ACCEPTED body
<anchor: parse-connect>

Read the assigned player id + server cookie from a `CONNECTION_REQUEST_ACCEPTED` body.

Verified against samp.dll sub_1000AA20: the body (after the RakNet id byte) is `[u32 external IP]
[u16 port][u16 systemIndex][u32 cookie]`. The systemIndex is the assigned local player id; the
cookie XORed with `crate::CHALLENGE_XOR` (0xFD9) becomes the `ClientJoin` challenge response.

---
