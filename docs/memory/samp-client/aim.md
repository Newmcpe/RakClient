# samp-client/aim.rs

## Module (native aim-sync emulation)
<anchor: module>

Native aim-sync emulation, ported from `aim_fix_updated.lua`.

A real SA-MP client streams an aim-sync packet describing where its camera points. A bare bot
never sends one, which looks unnatural. This makes the client periodically (every random 5–60 s,
and only while standing still) send a believable aim packet: a camera placed ~2 units behind the
bot per its facing, jittered by a small random offset, looking back at the bot. When the server
repositions the bot (`SetPlayerPos`/camera RPCs) the aim is regenerated and sent promptly.

The aim it produces is the high-level [`AimData`]; the driver mirrors it into
[`crate::state::LocalPlayer::aim`] and (with client emulation) tweaks it before [`AimSync::encode`]
packs it into the wire [`AimSyncData`].

## AimSync (usage)
<anchor: aimsync-usage>

Aim-sync state machine. Construct with [`AimSync::new`], feed it position updates and server
repositions, then each sync tick poll [`AimSync::due`] and, when due, [`AimSync::encode`].

## due
<anchor: due>

Whether an aim send is due (and the bot is not moving), rescheduling the next send. Moving
consumes the move flag and skips this cycle. When this returns `true`, mutate [`Self::aim_mut`]
if desired and then call [`Self::encode`].

---
