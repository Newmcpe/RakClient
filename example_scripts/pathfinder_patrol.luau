--!strict
-- Pathfinding patrol — walk a real-player route over the loaded navmesh.
--
-- Demonstrates the native navmesh walker: `walkTo(x, y, z, gait)` plans a path over the `.nav` loaded at
-- startup and moves the bot with a real GTA-SA gait (walk/jog/sprint speed + forward analog + movement
-- keys + animation, reversed from a live on-foot 207 capture), so to other players it reads as a person
-- walking, not a teleport. The script loops a list of waypoints, pausing at each — a simple patrol.
--
-- Run the client with `--navmesh <file.nav>` (e.g. the bundled `sawmill.nav`); WITHOUT a navmesh `walkTo`
-- has no path to plan and the bot won't move. Edit WAYPOINTS to your own route (SA coordinates), pick a
-- gait, and it auto-starts on spawn.

-- The route, in SA world coordinates. Defaults trace a loop around the sawmill (works with sawmill.nav).
local WAYPOINTS: { { x: number, y: number, z: number } } = {
    { x = -506.06, y = -190.55, z = 78.25 },
    { x = -531.59, y = -161.02, z = 78.15 },
    { x = -546.70, y = -134.04, z = 72.10 },
    { x = -578.82, y = -153.92, z = 77.79 },
}
local GAIT = "jog" -- "walk" (1.6 u/s) | "jog" (5.7) | "sprint" (9.6)
local PAUSE_MS = 1500 -- linger at each waypoint before moving on

local running = false

-- Walk to (x, y, z) over the navmesh and BLOCK until arrival (or the walk aborts). The driver owns the
-- path and gait; we only poll isWalking(). Returns false if the script was stopped mid-walk.
local function walkToBlocking(x: number, y: number, z: number): boolean
    walkTo(x, y, z, GAIT)
    -- Give the driver a few ticks to plan the path and start moving.
    local started = false
    for _ = 1, 6 do
        wait(100)
        if isWalking() then
            started = true
            break
        end
    end
    if not started then
        return running -- already within arrival range, or no navmesh/path from here
    end
    while running and isWalking() do
        wait(150)
    end
    return running
end

local function patrol()
    while running do
        for _, wp in ipairs(WAYPOINTS) do
            if not running then
                return
            end
            print(("[patrol] -> %.1f, %.1f, %.1f"):format(wp.x, wp.y, wp.z))
            walkToBlocking(wp.x, wp.y, wp.z)
            local px, py, pz = getBotPosition()
            print(("[patrol] arrived %.1f, %.1f, %.1f"):format(px, py, pz))
            wait(PAUSE_MS)
        end
    end
end

registerHandler("onSpawn", function()
    if running then
        return
    end
    running = true
    print("[patrol] spawned — starting navmesh patrol (" .. GAIT .. ")")
    newTask(patrol)
end)

registerHandler("onDisconnect", function()
    running = false
    walkStop()
end)
