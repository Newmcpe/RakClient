--!strict
-- Bundled "fly to coords" movement API for the headless bot. Fully typed against `host.d.luau`
-- (the host-globals definition file), so it type-checks under `luau analyze --definitions=host.d.luau`.
--
-- A headless bot has no GTA physics, so we craft the on-foot sync directly — but the server's
-- anti-cheat measures our reported position deltas, and a moving position with a ZERO move_speed
-- reads as a teleport/speed-hack (the dialog 19999 "подозрение в читерстве"). Ported from the real
-- launcher's "Вжух" coordmaster: each tick we advance the position toward the target AND report a
-- plausible velocity vector (move_speed) plus a walk animation, both ramped up from zero and
-- jittered, so the delta is backed by a matching velocity instead of a jump.
--
-- `fly.to(x, y, z, opts)` BLOCKS the calling coroutine (uses `wait()`), so call it from inside a task
-- (`newTask`). It returns true on arrival, false if `fly.stop()` interrupted it. Flying only has an
-- effect once the bot is Spawned (`updateSync` is a no-op before that); `fly.to` waits for spawn.

require("addon") -- wait()/newTask scheduler

-- The full movement-tuning shape (all fields required); `FlyOpts` is its all-optional public form.
type FlyConfig = {
    step: number, -- position units advanced per tick at full ramp
    tick: number, -- ms between on-foot syncs
    speedMag: number, -- reported move_speed magnitude at full ramp
    ramp: number, -- per-tick ramp of step+velocity
    animId: number, -- on-foot walk animation index
    animFlags: number, -- on-foot walk animation flags
    arrive: number, -- stop when within this many units of the target
}

export type FlyOpts = {
    step: number?,
    tick: number?,
    speedMag: number?,
    ramp: number?,
    animId: number?,
    animFlags: number?,
    arrive: number?,
}

-- The module's public API shape.
export type Fly = {
    distance: (x: number, y: number, z: number) -> number,
    waitSpawn: () -> (),
    stop: () -> (),
    to: (tx: number, ty: number, tz: number, opts: FlyOpts?) -> boolean,
}

local fly: Fly = {} :: Fly

-- Speeds match the reference "Вжух" coordmaster's on-foot LongSettings exactly (Pace 8, wait 230 ms,
-- move_speed 0.9): ~35 u/s with a reported velocity of 0.9 that stays consistent with the position
-- delta. This is the known-good bypass — step 9 (my earlier over-fast value) tripped Arizona's
-- "Speed Hack - Пешком(9/1)" kick (the "9" was the detected step); 8 is under the threshold.
local DEFAULTS: FlyConfig = {
    step = 8, -- Pace: units advanced per tick at full ramp (~35 u/s at tick 230)
    tick = 230, -- ms between on-foot syncs (reference WaitOnFoot)
    speedMag = 0.9, -- reported move_speed magnitude at full ramp (reference on-foot LongSettings[4])
    ramp = 0.25, -- per-tick ramp of step+velocity — fast accel so SHORT tree-to-tree hops reach speed
    -- quickly instead of crawling through the ramp (max speed is still step 8, the AC-safe cap)
    animId = 1189,
    animFlags = 7684,
    arrive = 2.0,
}

-- A shared flag so `fly.stop()` (or a new `fly.to`) interrupts an in-flight move.
local flying = false

local function jitter(scale: number): number
    return (math.random(-1000, 1000) / 1000) * scale
end

-- Straight-line distance from the bot to (x, y, z).
function fly.distance(x: number, y: number, z: number): number
    local bx, by, bz = getBotPosition()
    local dx, dy, dz = x - bx, y - by, z - bz
    return math.sqrt(dx * dx + dy * dy + dz * dz)
end

-- Block until the bot is spawned (position is non-zero); returns immediately if already spawned.
function fly.waitSpawn()
    while true do
        local x, y, z = getBotPosition()
        if not (x == 0 and y == 0 and z == 0) then
            return
        end
        wait(300)
    end
end

-- Interrupt any in-progress `fly.to` and clear the reported velocity/animation.
function fly.stop()
    flying = false
    setBotVelocity(0, 0, 0)
    setBotAnimation(0, 0)
    updateSync()
end

-- Blocking move to (x, y, z). Returns true on arrival, false if interrupted by `fly.stop()`.
function fly.to(tx: number, ty: number, tz: number, opts: FlyOpts?): boolean
    local o: FlyOpts = opts or {}
    local step = o.step or DEFAULTS.step
    local tick = o.tick or DEFAULTS.tick
    local speedMag = o.speedMag or DEFAULTS.speedMag
    local rampRate = o.ramp or DEFAULTS.ramp
    local animId = o.animId or DEFAULTS.animId
    local animFlags = o.animFlags or DEFAULTS.animFlags
    local arrive = o.arrive or DEFAULTS.arrive

    fly.waitSpawn()
    flying = true
    local ramp = 0.3 -- 0 → 1, scales both position step and reported velocity; start at 0.3 (not 0) so
    -- a short hop moves at once instead of inching out of a standstill (velocity stays matched → AC-safe)
    local sx, sy, sz = getBotPosition()
    print(("[fly] start %.1f,%.1f,%.1f -> %.1f,%.1f,%.1f"):format(sx, sy, sz, tx, ty, tz))
    local lastLog = os.clock()

    while flying do
        local x, y, z = getBotPosition()
        -- Abort if the client dropped/reset (position zeroes on reconnect): don't keep driving a
        -- stale flight while disconnected.
        if x == 0 and y == 0 and z == 0 then
            flying = false
            print("[fly] aborted — not spawned (disconnected?)")
            return false
        end
        local dx, dy, dz = tx - x, ty - y, tz - z
        local d = math.sqrt(dx * dx + dy * dy + dz * dz)

        if os.clock() - lastLog >= 2 then
            -- ETA = remaining distance / full-ramp speed (step units per tick → units/second).
            local speed = step / (tick / 1000)
            local eta = if speed > 0 then d / speed else 0
            print(("[fly] pos %.1f,%.1f,%.1f | dist %.1f | ETA %.0fs"):format(x, y, z, d, eta))
            lastLog = os.clock()
        end

        if d <= math.max(arrive, step) then
            setBotPosition(tx, ty, tz)
            setBotVelocity(0, 0, 0) -- stop moving: no reported velocity, drop the anim
            setBotAnimation(0, 0)
            updateSync()
            flying = false
            print(("[fly] arrived %.1f,%.1f,%.1f"):format(tx, ty, tz))
            return true
        end

        if ramp < 1 then
            ramp = math.min(1, ramp + rampRate)
        end

        local inv = 1 / d
        local ux, uy, uz = dx * inv, dy * inv, dz * inv -- unit direction toward target

        -- Advance the reported position by the ramped step (with a little jitter on Z).
        local advance = step * ramp
        setBotPosition(x + ux * advance, y + uy * advance, z + uz * advance + jitter(0.05))

        -- Report a velocity in the travel direction (ramped + jittered) so the position delta is
        -- backed by a matching move_speed rather than reading as a teleport, plus the walk animation.
        local mag = speedMag * ramp
        setBotVelocity(ux * mag + jitter(0.02), uy * mag + jitter(0.02), uz * mag + jitter(0.02))
        setBotAnimation(animId, animFlags)
        updateSync()

        wait(tick)
    end

    return false -- interrupted by fly.stop()
end

return fly
