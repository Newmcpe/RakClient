# samp-proto/sync.rs

## Module — two sync encoding families
<anchor: module>

Native typed sync-packet structs, replacing the LuaJIT `ffi` structs in `synchronization.lua`.

Two encoding families are represented here:
- **Bit-packed** sync (`PlayerSyncData`, `VehicleSyncData`): the SA-MP sender uses the bitstream
  field helpers (optional-value bool flags, `normQuat`, `compressedVector`, compressed
  health/armor) — ported from `events/handlers.lua` `packet_player_sync_*`/`packet_vehicle_sync_*`.
- **Raw struct** sync (`AimSyncData`, `BulletSyncData`, `TrailerSyncData`, `UnoccupiedSyncData`,
  `PassengerSyncData`): SA-MP reads/writes these as a flat byte buffer (`read_sync_data`), so each
  field is a plain byte-aligned read in declaration order, exactly matching the packed C structs.
  Bitfield bytes (e.g. `camExtZoom:6|weaponState:2`) are kept as the raw combined `u8`.

---

## VehicleSyncData — field-name provenance & unmodeled preamble
<anchor: vehicle-sync>

In-vehicle driver sync (`ID_VEHICLE_SYNC` = 200). `train_speed` and `trailer_id` are bool-flagged
optionals in the SA-MP reader.

Field-name provenance — binary-verified against `SAMP_Packet_VehicleSync` @ samp.dll `0x1000A6B0`.
Field names are the binary-verified truth (MoonLoader/SAMP.Lua legacy names in parentheses):

- `siren`: wire u8; only the low 6 bits (`& 0x3F`) are meaningful — no weapon field (MoonLoader: `currentWeapon`).
- `landing_gear`: 1-bit flag (MoonLoader: `siren`).
- `unknown_flag`: unidentified 1-bit flag (MoonLoader: `landingGear`).

The luau MoonLoader-compat reader keeps the legacy names; this Rust struct uses the accurate ones.
Wire layout is unchanged (u8 + bit + bit).

NOT modeled: the SA-MP handler tolerates an optional leading `ID_TIMESTAMP` (`0x28`) preamble
(`[0x28][u32 seq]`, a stale-packet counter) before the body. The transport strips `ID_TIMESTAMP`
on the RPC path but not on sync packets, so an inbound 200 carrying it would desync this decoder
— harmless for the on-foot chat bot, which does not decode inbound vehicle sync.

---
