--!nonstrict
-- Bundled Arizona packet mechanics — ABSTRACT.
--
-- Wire SHAPES of the Arizona CEF / validation packets (id 220), byte-matched to a real captured
-- launcher handshake. Holds no fixed identity: the auth key / app version / nick live in the USER
-- script that requires this (see example_scripts/arizona_launcher_emulation.luau); `userId` and
-- `clientId` are random per run. Set `arizona.host` to the server host before `validation` runs.

require("addon") -- newTask/wait scheduler used by validation()
local packet = require("packet")

local AZ = 220 -- Arizona CEF / validation packet id

local arizona = {}

-- Server host written into the 220/140 command line; the user script sets this from its config.
arizona.host = ""

-- A random 64-char lowercase-hex id (Arizona's getUniqueUserId).
function arizona.uniqueUserId(): string
    local chars = "0123456789abcdef"
    local id = table.create(64)
    for i = 1, 64 do
        local n = math.random(1, #chars)
        id[i] = string.sub(chars, n, n)
    end
    return table.concat(id)
end

-- Random identity, generated once per run and reused across the handshake. The real client keeps
-- both stable across reconnects (they are persisted launcher tokens); we mint fresh ones per launch.
arizona.userId = arizona.uniqueUserId() -- the launcher -userId token carried in 220/140
arizona.clientId = arizona.uniqueUserId() -- the 220/38 client id

-- 220/18 CEF/Svelte message: [220][18][u16 len][text][u32 0].
function arizona.sendCef(text: string)
    packet(AZ, 18):string16(text):u32(0):sendEx(1, 9, 2)
end

-- 220/140 launcher command line: [220][140][u16 len][cmdline]. Carries the -userId session token.
-- Field order/flags byte-matched to the captured launcher; nick comes from the host getter.
function arizona.gamePath(host: string?)
    local path = string.format(
        '"gta_sa.exe"  -c -h %s -p 7777 -n %s -mem 2048 -window -modern_scale -widescreen -trees_new -enable_grass -arizona -userId %s -cdn 1,1,1 -referrer ',
        host or arizona.host, getBotNick(), arizona.userId
    )
    packet(AZ, 140):u16(#path):text(path):sendEx(1, 9, 2)
end

-- The post-join validation set, in the real client's order: 220/140 command line, 220/20 resolution,
-- 220/50 focus, 220/38 client id, then the CEF onSvelteAppInit. Waits briefly first like the launcher.
function arizona.validation()
    wait(200)
    arizona.gamePath()
    packet(AZ, 20):u32(sampResolutionW or 1920):u32(sampResolutionH or 1080):sendEx(1, 9, 2)
    packet(AZ, 50):u8(1):sendEx(1, 9, 2)
    packet(AZ, 38):text(arizona.clientId):sendEx(1, 9, 2)
    arizona.sendCef("onSvelteAppInit")
end

-- CEF liveness heartbeat. After spawn the server pokes the real client's browser with 220/17
-- `executeEvent` calls and expects a 220/17 ack echoing the request id (the trailing u32). A headless
-- bot must answer every one or the server's watchdog rejects it with "ОШИБКА 7721" ~5s after spawn.
-- Reply mirrors the real client: [220][17][u32 0][u32 requestId]. The packet bitStream includes the
-- leading [220] id byte (chokepoint convention), so sub-id is at byte 1.
-- Layout: [220][17][u32 0][u16 payloadLen][payload][u32 requestId][optional trailing byte].
-- Parse the requestId FORWARD from payloadLen — the binary `executeEvent` variants emitted during the
-- auth/dialog flow carry a trailing byte after the id, so reading "last 4 bytes" echoes a corrupt id
-- and the server's watchdog kills the session with "ОШИБКА 7721".
local function replyCefHeartbeat(id: number, bs)
    if id ~= AZ then return end
    local total = bs:getNumberOfUnreadBytes()
    if total < 12 then return end
    bs:setReadOffset(8) -- skip the [220] packet-id byte
    if bs:readUInt8() ~= 17 then return end
    bs:setReadOffset(6 * 8) -- the u16 payload length (after [220][17][u32 0])
    local payloadLen = bs:readUInt16()
    if total < 8 + payloadLen + 4 then return end
    bs:setReadOffset((8 + payloadLen) * 8) -- the u32 requestId follows the payload
    packet(AZ, 17):u32(0):u32(bs:readUInt32()):sendEx(1, 9, 2)
end

registerHandler("onReceivePacket", replyCefHeartbeat)

return arizona
