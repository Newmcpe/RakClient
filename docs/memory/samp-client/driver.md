# samp-client/driver.rs

## Module (connection state machine)
<anchor: module>

The connection state machine.

[`Driver`] is generic over a [`Transport`] so it can be unit-tested against a scripted fake while
production instantiates it with the real RakNet transport. Packet bodies are encoded/decoded
through the [`samp_proto::Encode`]/[`samp_proto::Decode`] traits on each packet struct. It is
pumped one event at a time through [`Driver::next_event`], which internally `select!`s over
incoming transport events, the on-foot sync interval (while `Spawned`), and the reconnect timer.

## IDLE_SYNC_INTERVAL
<anchor: idle-sync-interval>

When the bot's on-foot state is unchanged, resend a keepalive sync at most this often instead of
every `sync_interval` tick. Matches the real client's floor: samp.dll's on-foot/aim senders
(`sub_10004D40`/`sub_10005040`) force a resend once `GetTickCount - last > 0x1F4` (500 ms) even when
the packet is byte-identical, so a stationary player still reports at ≥2 Hz.

## STATS_INTERVAL
<anchor: stats-interval>

How often the client reports its stats (`PACKET_STATS_UPDATE` = 205, money + drunk) while spawned.
The real client sends this every second (`NetGame_Process` gate `TickCount - last > 0x3E8`).

## SYNC_CHANNEL
<anchor: sync-channel>

RakNet ordering channel for all player sync (on-foot 207, aim 203, spectator 212). The real client
sends sync on channel 1, keeping it off channel 0 which carries the reliable-ordered RPC stream —
verified against samp.dll's sync senders (`sub_10004D40`/`sub_10005040`/`sub_10006320`, all
`Send(..., orderingChannel = 1)`). We previously sent sync on channel 0, mixing it with RPCs.

## FALLBACK_SPAWN_POSITION
<anchor: fallback-spawn-position>

Fallback spawn position for [`Driver::enter_spawned`] when the server never sent `SetSpawnInfo`
(the self-spawn fallback path). Decoded from a real captured `SetSpawnInfo` body on Arizona's
Bumble Bee server (`[u8 team=255][u32 skin][u8 unused][f32 x,y,z,zAngle]...`, floats at byte
offsets 6/10/14/18) rather than defaulting to the map origin.

## Gait (struct)
<anchor: gait>

A locomotion gait, reverse-engineered from a live GTA-SA/Arizona on-foot (207)
capture of CJ walking / jogging / sprinting (median over 397 outbound syncs).
The receiving clients animate a remote player from `up_down` + `keys` +
`move_speed`, so matching all three reproduces the real walk/run/sprint look
and speed — unlike the old fly.luau coordmaster (a fixed ~35 u/s fast-travel,
~3.7× real sprint, one static anim).

## Gait::move_speed_mag (field)
<anchor: gait-move-speed-mag>

Reported `move_speed` vector magnitude. MUST equal `speed / 50`: the remote client does NOT lerp
our position — it runs the ped physically at this velocity (units per 1/50 s frame) and only
hard-SNAPS to our synced position when the error exceeds 2 m XY / 1 m Z (samp.dll
`CRemotePlayer_OnFoot_PosCorrect_NudgeOrSnap`). If this magnitude disagrees with our real advance
rate, the remote's dead-reckoning drifts a few cm/s until it crosses 2 m and teleports.

## GAIT_WALK
<anchor: gait-walk>

Slow walk (KEY_WALK held): 1.556 u/s. Engine ground truth — WALK_player anim root motion 0.0311
u/frame (gta_sa.exe `CPed::CalculateNewVelocity` ÷ `ms_fTimeStep`). The capture's 0.066 was the
mobile half-stick blend (no walk key), NOT PC alt-walk; holding KEY_WALK the engine gives 0.0311.
Locomotion anim is 1231 for walk/jog/sprint alike (only idle is 1189); gaits differ by speed+keys.
(`move_speed_mag` 0.0311 = 1.556 / 50 — must track `speed` or the remote drifts then snaps.)

## GAIT_JOG
<anchor: gait-jog>

Default jog (push forward, no modifier): 5.7 u/s. Engine run_player root motion 0.11413 u/frame
(matches the capture's jog median ~0.115). `move_speed_mag` 0.114 = 5.7 / 50.

## GAIT_SPRINT
<anchor: gait-sprint>

Sprint (KEY_SPRINT held): 9.56 u/s. Engine sprint_civi root motion 0.19124 u/frame at the hold rate
(button-mashing ramps to ~0.249 / 12.4 u/s; we hold the key, so we use the steady value).
Same locomotion anim 1231 as walk/jog; sprint carries anim flag 0x8002 in the capture.
`move_speed_mag` 0.1912 = 9.56 / 50.

## WalkState (struct)
<anchor: walkstate>

Native navmesh walk in progress. Advances the position at the gait's real world
speed (time-based, so any sync cadence is smooth), reporting the matching
analog/keys/move_speed/anim so it reads as a genuine walk/jog/sprint.

## WalkState::heading (field)
<anchor: walkstate-heading>

Current facing/travel heading (SA φ = atan2(dx, dy)), slewed toward the waypoint bearing at
`TURN_RATE` so corners rotate smoothly instead of snapping. `None` until the first move tick,
which snaps to the initial bearing (no spin on start).

## WalkState::speed_frac (field)
<anchor: walkstate-speed-frac>

Current fraction (0..1) of the gait's full speed. Ramps up from 0 at `WALK_ACCEL` so the ped
accelerates from a standstill like the real client instead of snapping to full speed (a jerk the
remote sees on every short hop), and is braked down approaching the target so it coasts to a stop.

## TURN_RATE
<anchor: turn-rate>

Max facing turn rate while walking, rad/s. The real client rotates its ped smoothly through a path
corner (measured ~6–12°/frame on gentle arcs, up to ~34°/frame ≈ 450°/s on hairpins — never an
instant snap). Slewing our heading at this rate makes waypoint transitions read as a natural turn
instead of the violent facing/velocity jerk a one-tick 90° snap produced. dt-scaled per tick.
Value: 7.85 ≈ ~450°/s, the real client's observed hairpin rate.

## WALK_ACCEL
<anchor: walk-accel>

Speed ramp-up rate, fraction-of-gait per second. The real ped accelerates from a standstill (its
moveBlendRatio ramps ~0.07/frame ≈ 3.5/s); we reach full gait in ~0.5 s so short hops look like a
real run-up instead of an instant full-speed snap (the jerk remote clients saw). Value 2.0.

## WALK_ARRIVE
<anchor: walk-arrive>

Stop when within this many units of the final target. Kept well under the remote client's 2 m XY /
1 m Z snap threshold (samp.dll `CRemotePlayer_OnFoot_PosCorrect_NudgeOrSnap`): the walker snaps the
last sub-`WALK_ARRIVE` gap to the exact target in one sync, so at 2.0 that final jump read as a
teleport. At 0.5 the remote absorbs it with its per-frame nudge instead. Value: 1.0.

## PED_GROUND_OFFSET
<anchor: ped-ground-offset>

Ped-origin height above the navmesh floor: measured ~0.68–0.91 m against known sawmill ped coords
(RENT_SPOT +0.68, SAWMILL_ANCHOR +0.91, CONVEYOR +0.74, tree +0.77). NOT applied — our reconstructed
navmesh z sits BELOW the real server floor, so adding this lifted the ped ABOVE the ground in places
and Arizona's anti-cheat kicked it "Fly Hack - Пешком(7/1)". Travelling on the (slightly sunk) nav
floor is AC-safe; the correct fix is fixing the navmesh z at bake time, not a runtime lift that can
hover. Kept for that future work — see the git history / `arizona-onfoot-sync-ground-truth` memory.

## Driver::arizona_streamer_id (field)
<anchor: arizona-streamer-id>

Streamer entity id the server assigns us via inbound `221/113` (`[0xDD][0x71][0x00][u16 id]`).
Our outbound `221/53` sync must report THIS id, not our SA-MP player id, or the server's
streamer watchdog treats the entity's sync as ignored ("Игнорирование функции(52 / 1)").

## flush_outbox
<anchor: flush-outbox>

Send everything scripts queued via `sampSendPacket`/`sampSendRpc` since the last drain, each
with the reliability/channel the script asked for (`sendPacket()` defaults to
reliable-ordered on channel 0 — the Arizona `220` path). Outgoing packets pass through the
`onSendPacket` chokepoint first.

## set_standing (idle-anim fingerprint + state clear)
<anchor: set-standing>

Idle stance anim, NOT (0,0): the real Arizona client sends (1189, 0x8004) even while standing
perfectly still — (0,0) appears in ZERO of 11,965 captured on-foot packets, so reporting it is
a trivial headless fingerprint.

Also clear the shared local-player state. `mirror_from_state` pulls move_speed/keys/animation
from it every tick, so a walk velocity left behind would leak straight back into the idle
keepalive syncs and make the stationary bot drift ("дёргается") for other players. The walker
owns these fields while it runs; on stop the bot is genuinely idle until a script drives it.

## step_walk: speed ramp/brake
<anchor: step-walk-ramp>

Ramp speed up from a standstill (accelerate) and brake it down near the final target, so the
ped runs up and coasts to a stop like the real client instead of snapping to/from full speed
(the jerk remote clients saw on short hops). `frac` scales BOTH the position advance and the
reported move_speed, so the speed=move_speed×50 invariant holds at every ramp point (AC-safe).

## step_walk: waypoint retarget (horizontal distances)
<anchor: step-walk-retarget>

Retarget: skip waypoints already within this step's reach. Distances are HORIZONTAL — z is
tracked separately (straight to target.z), so mixing the ~0.8 m nav-vs-real z gap into the
waypoint distance would keep the last waypoint permanently out of `WALK_ARRIVE` reach.

## step_walk: bearing slew
<anchor: step-walk-slew>

Bearing toward the current waypoint, then SLEW the actual travel heading toward it at TURN_RATE
rather than snapping. Driving position, facing, and move_speed all from this one slewed heading
keeps them perfectly consistent through a corner — the real client turns this way (facing +
velocity rotate together over 2–3 frames), so remote peds see a smooth arc, not a one-tick jerk.

## step_walk: z-tracking along grounded path
<anchor: step-walk-z>

Track z toward the CURRENT waypoint's grounded z, in proportion to horizontal progress toward
it. `find_path` now densifies each leg and lifts every intermediate waypoint onto the detail
mesh (see sa-nav `ground_polyline`), so following waypoint z hugs the terrain instead of cutting
a straight line to the final target — the old straight-line-to-target z dived under any rise
between here and the goal, which to other players read as a teleport/no-clip through the ground.
The nav floor sits ~0.8 m below the real server floor, but that offset is CONSTANT along the
grounded path (not a dive through a hill), and travelling on the slightly sunk nav floor is
AC-safe; it was the vertical drop-THROUGH-terrain, not the small constant sink, that tripped
"Fly Hack - Пешком(7/1)".

## step_walk: gait report + fly-hack root cause
<anchor: step-walk-gait>

Report the real gait: forward analog + movement keys + a matching 3-D velocity + the gait
animation. This reads as a genuine walking/sprinting player now that the ACTUAL fly-hack bug is
fixed: the codec was serializing the quaternion as (x,y,z,w) but the wire is (w,x,y,z), so every
facing quat decoded server-side as a ped tilted ~30° off vertical — the literal fly pose — which
is what tripped "Fly Hack - Пешком(7/1)" on every walk. With the codec order corrected, an
upright ped that faces its travel direction and walks is AC-valid.

Facing quat: face the (slewed) travel heading. SA φ = atan2(dx, dy); the on-wire quaternion about
world +Z is (w=cos(φ/2), x=0, y=0, z=sin(φ/2)) → forward=(dx, dy). Serialized w-first by the codec.

Velocity: tracks the true 3-D motion — a moving position with a zero move_speed reads as a
teleport (dialog 19999). z = grade × gait magnitude so |xy| stays full gait speed and z matches
the climb/descent.

## send_prespawn_poll (no periodic RequestClass / heartbeat)
<anchor: send-prespawn-poll>

Pre-spawn tick while spectating: only the optional self-spawn fallback runs here. A real
client sends neither a periodic `RequestClass` re-request nor an automatic `UpdateScoresPingsIPs`
heartbeat while waiting to spawn (live capture: `RequestClass` never appears in a real client's
outbound traffic at all, and `UpdateScoresPingsIPs` only fires once, user-triggered by opening
the scoreboard, hours into a session — never automatically near join). Sending either on a
steady 1s cadence during the exact window the server decides whether to grant a spawn is a
distinctive non-human traffic pattern a server could key an anti-bot check on, so we don't.

Body: the self-spawn fallback (`config.self_spawn_timeout`) is opt-in and OFF by default. On
Arizona an unauthorised RPC_Spawn (no server `SetSpawnInfo`) trips the anti-cheat, which
kicks with "подозрение в читерстве" ~60s later — verified live. A spectating client is not
flagged and still receives chat/world state, so staying spectating is the safe default; the
fallback exists only for non-Arizona servers that legitimately never drive the spawn.

## on_class_response: spawn-only-after-spawn-info
<anchor: on-class-response>

RakSAMP Lite (`network_transport.cpp`): on a class response, if the server has already sent
`SetSpawnInfo` (i.e. login is done), request spawn now — the server answers with
`RequestSpawnResponse`, which spawns us. Before spawn info we keep spectating (requesting
spawn early earns "ОШИБКА 7721").

## on_spawn_response: allow condition
<anchor: on-spawn-response>

RakSAMP Lite's RPC_RequestSpawnResponse condition (0x45ace0): spawn when `allow == 2`, or
when `allow != 0` while we explicitly requested spawn. On Arizona the server sends
`allow == 2` after login — so this is the server-driven spawn, no explicit client call.

## on_set_spawn_info: re-spawn on post-skin SetSpawnInfo
<anchor: on-set-spawn-info-respawn>

The real client answers EVERY SetSpawnInfo with RPC 52 (Spawn) — including the SECOND one the
server sends right after a fresh account confirms its skin (`chooseSelector.buy`). That second
Spawn is what moves the character out of the ChooseSelector room (interior 211) into the world;
NOT sending it is exactly the "Игнорирование функции(52 / 1)" anti-cheat kick. The FIRST
SetSpawnInfo arrives while still spectating (not yet Spawned) and is driven by the normal
class/spawn-response path, so only re-send when we're ALREADY Spawned (the post-skin / respawn
case). Re-send Spawn and adopt the new position, but don't re-fire the Spawned event — the FSM
is already Spawned and the scripts' onSpawn already ran.

## on_toggle_spectating (RPC 124)
<anchor: on-toggle-spectating>

`TogglePlayerSpectating` (RPC 124): the Arizona server toggles spectate ON during login and
OFF afterwards. A `1 → 0` transition means "drop out of spectate" → spawn, exactly like
RakSAMP Lite's `sub_45B260 → Net_Spawn`. This is the server-driven spawn, no explicit call.

Spectate-on arms the self-spawn fallback timer even when no class response will ever arrive
(an Arizona script drops the automatic RequestClass, so the server's spectate-on is then the
only signal that the pre-spawn window has begun).

On the `1 → 0` transition: RakSAMP Lite (`network_transport.cpp`): a `1 → 0` spectate toggle
routes through `RequestSpawn` (RPC 129), NOT a direct `Spawn` (RPC 52). The server then replies
`RequestSpawnResponse` with `allow != 0`, which drives `enter_spawned`. Sending an unsolicited
`Spawn` (skipping the RequestSpawn→allow handshake) is what Arizona's anti-cheat flags as
"Игнорирование функции(52 / 1)" and kicks ~16s after spawn. Stay spectating until the server's
spawn grant arrives.

## enter_spawned (spawn convergence point + triggers)
<anchor: enter-spawned>

Send `RPC_Spawn` and enter the `Spawned` state — the single convergence point for every spawn
trigger. We spectate (spectator-sync 212) after join until one of these fires (the only
triggers the real RakSAMP Lite binary has — `Net_Spawn` xrefs at `0x455bb0`):
- `TogglePlayerSpectating` (RPC 124) on a `1 → 0` transition (`on_toggle_spectating`) — the
  Arizona post-login trigger — or `RequestSpawnResponse(allow==2)` (`on_spawn_response`).
- MANUAL: `sampSpawnPlayer()` sends `RequestSpawn`, whose response then spawns us.

Body: Net_Spawn copies the SetSpawnInfo position into the local sync position — this is where the
post-spawn position comes from (the class-response spawn_position is usually zero).

## send_spectator_sync: unreliable on sync channel
<anchor: send-spectator-sync>

The real client sends spectator sync UNRELIABLE (reliability 6, not sequenced) on the sync
channel — samp.dll `sub_10006320`, `Send(..., reliability = 6, orderingChannel = 1)`.

## send_arizona_221_sync
<anchor: send-arizona-221>

Stream Arizona's custom on-foot position report (packet 221, sub-id 53) alongside stock 207.
Arizona's anti-cheat kicks a client that never sends it ("Игнорирование функции(52 / 1)"). This
is the minimal stationary form: our own entity id + current position + a monotonic ms timestamp
+ the rest velocity/heading. Sent `UnreliableSequenced` on `SYNC_CHANNEL` like the other sync.

Report the server-assigned streamer id (from 221/113); fall back to our player id until it
arrives. Reporting the wrong id is what the streamer watchdog flags as "function 52".

## send_stats_update
<anchor: send-stats-update>

Report player stats (`PACKET_STATS_UPDATE` = 205: money + drunk) at the `STATS_INTERVAL`
cadence, matching the real client's 1 Hz stats send while spawned. Sent UNRELIABLE on channel 0
like the real client (`NetGame_Process` @0x10005B10); drunk level is always 0 (headless bot).

## on_disconnect: reconnect policy
<anchor: on-disconnect-reconnect>

Local quit (`DisconnectReason::Local`): never reconnect.

Terminal reasons (Banned / InvalidPassword): reconnecting is futile, and for a ban it just
hammers the server. Stop the client cleanly (closed → next_event yields None → the app loop
exits) instead of looping.

Everything else (kick / rejection / connection loss / timeout) is retried, but with a cap so a
server that keeps dropping us straight back out can't spin forever. A session that survived at
least STABLE_SESSION before dropping is treated as a one-off and resets the counter; a
never-stabilising loop (kicked before/at spawn, or spawn→instant-kick) keeps accumulating.

---
