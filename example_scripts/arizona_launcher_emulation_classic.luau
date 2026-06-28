--!nonstrict
-- Arizona launcher emulation — CLASSIC (low-level) style.
--
-- Every Arizona CEF / validation packet is built byte by byte with the raw bitStream
-- writers. This is the verbose, no-sugar reference for how the native API works; the
-- sugar version (arizona_launcher_emulation.luau) produces the exact same wire output.
--
-- Host context comes from getters the client injects: `sampResolutionW/H` (display
-- resolution) and `getBotNick()` — same as the old arizona.lua used.

require("addon")
local sampev = require("samp.events")

local function sendCef(text: string)
    local bs = bitStream.new()
    bs:writeInt8(220)
    bs:writeInt8(18)
    bs:writeInt16(#text)
    bs:writeString(text)
    bs:writeInt32(0)
    bs:writeInt16(0)
    bs:sendPacket()
    bs:reset()
end

local function uniqueUserId(): string
    local chars = "0123456789abcdef"
    local id = ""
    for _ = 1, 64 do
        local i = math.random(1, #chars)
        id ..= string.sub(chars, i, i)
    end
    return id
end

local function sendGamePath()
    local path = string.format(
        '"gta_sa.exe"  -c -h  -p 7777 -n %s -mem 2048 -window -x -widescreen -graphics -enable_grass -arizona -userId %s -cdn 0,0,0 -referrer',
        getBotNick(), uniqueUserId()
    )
    local bs = bitStream.new()
    bs:writeInt8(220)
    bs:writeInt8(140)
    bs:writeInt32(#path)
    bs:writeString(path)
    bs:writeInt8(0)
    bs:sendPacket()
    bs:reset()
end

local function validation()
    wait(1000)

    -- resolution (220/20): width, height as u32 LE, read from the host getters
    local bs = bitStream.new()
    bs:writeInt8(220)
    bs:writeInt8(20)
    bs:writeInt32(sampResolutionW or 1920)
    bs:writeInt32(sampResolutionH or 1080)
    bs:sendPacket()
    bs:reset()

    -- window focused (220/50)
    bs:writeInt8(220)
    bs:writeInt8(50)
    bs:writeInt8(1)
    bs:sendPacket()
    bs:reset()

    -- client id (220/38): 64 hex chars as raw bytes
    bs:writeInt8(220)
    bs:writeInt8(38)
    bs:writeString(uniqueUserId())
    bs:sendPacket()
    bs:reset()

    sendGamePath()
end

function sampev.onSendClientJoin(
    version: number, mod: number, nickname: string, challengeResponse: number,
    joinAuthKey: string, clientVer: string, challengeResponse2: number
)
    newTask(validation, 100)
    sendCef("onSvelteAppInit")
    sendCef("onSvelteAppVersion|1.0.0|ea94b29b")
    -- `joinAuthKey` is the driver's random `generate_gpci` (a valid ×1001 token) — no hardcoded key.
    return { 4057, 1, nickname, challengeResponse, joinAuthKey, "Arizona PC", challengeResponse2 }
end
