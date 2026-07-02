--!nonstrict
-- Bundled Arizona CEF / custom-packet API — ABSTRACT (holds no fixed identity).
--
-- A clean facade over the Arizona id-220 custom-packet channel: named sub-ids, a CEF message/event
-- API, and the post-join validation sequence. Wire bytes are byte-matched to a real captured launcher
-- handshake; the auth key / app version / nick live in the USER script that requires this (see
-- example_scripts/arizona_launcher_emulation.luau). `userId`/`clientId` are random per run; set
-- `arizona.host` before `validate()` runs. Every packet goes out on channel 2 (priority 1,
-- reliability 9) via `packet:sendEx`, matching the real client.

require("addon") -- newTask/wait scheduler used by validate()
require("arizona.type3") -- registers the RPC 186→187 anti-cheat attestation responder
local packet = require("packet")

local AZ = 220 -- Arizona CEF / custom-packet id

-- Named id-220 sub-ids — so scripts never use magic numbers.
local SUB = {
    executeEvent = 17, -- server→client CEF poke (we ack + dispatch by event name)
    cefMessage = 18, -- client→server CEF/Svelte message (onSvelteAppInit, ...)
    resolution = 20, -- screen resolution
    clientId = 38, -- 64-hex client id
    focus = 50, -- window focus flag
    commandLine = 140, -- launcher command line (carries -userId)
}

local arizona = {}
arizona.SUB = SUB

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

-- Random identity, fresh per launch. The real client persists these, but the server only enforces the
-- formats (not a machine match), so fresh randoms connect fine. Override the fields to pin them.
arizona.userId = arizona.uniqueUserId() -- the launcher -userId token carried in 220/140
arizona.clientId = arizona.uniqueUserId() -- the 220/38 client id

--------------------------------------------------------------------------------
-- CEF facade — 220/18 outbound message, 220/17 inbound event.
--------------------------------------------------------------------------------
arizona.cef = {}

-- Send a CEF/Svelte message: [220][18][u16 len][text][u32 0].
function arizona.cef.send(text: string)
    packet(AZ, SUB.cefMessage):string16(text):u32(0):sendEx(1, 9, 2)
end

-- Subscribe to a server-pushed CEF event. The server pokes the browser with 220/17
-- `window.executeEvent('<name>', '<args>')`; the handler is called as `fn(args, requestId)`. The
-- heartbeat ack is sent automatically whether or not a handler is registered.
local cefHandlers: { [string]: (string, number) -> () } = {}
function arizona.cef.on(eventName: string, fn: (string, number) -> ())
    cefHandlers[eventName] = fn
end

--------------------------------------------------------------------------------
-- Individual validation packets (each is also sent in order by `validate()`).
--------------------------------------------------------------------------------

-- 220/20 resolution: [220][20][u32 width][u32 height].
function arizona.resolution(width: number, height: number)
    packet(AZ, SUB.resolution):u32(width):u32(height):sendEx(1, 9, 2)
end

-- 220/50 window focus: [220][50][u8 focused].
function arizona.focus(focused: boolean)
    packet(AZ, SUB.focus):u8(focused and 1 or 0):sendEx(1, 9, 2)
end

-- 220/38 client id: [220][38][64-hex ascii].
function arizona.sendClientId()
    packet(AZ, SUB.clientId):text(arizona.clientId):sendEx(1, 9, 2)
end

-- 220/140 launcher command line: [220][140][u16 len][cmdline]. Carries the -userId session token.
-- Field order/flags byte-matched to the captured launcher; nick comes from the host getter.
function arizona.gamePath(host: string?)
    local path = string.format(
        '"gta_sa.exe"  -c -h %s -p 7777 -n %s -mem 2048 -window -modern_scale -x -widescreen -trees_new -enable_grass -arizona -userId %s -cdn 1,1,1 -referrer ',
        host or arizona.host,
        getBotNick(),
        arizona.userId
    )
    packet(AZ, SUB.commandLine):u16(#path):text(path):sendEx(1, 9, 2)
end

-- The post-join validation set, byte-matched to a real captured session's order: command line,
-- resolution, focus, client id, then the view-changed + app-init CEF pair. clientId (220/38) is sent
-- LAST of the four, not second — verified against a live MoonLoader capture (gamePath→resolution→
-- focus→clientId). Waits ~1s first like the launcher.
function arizona.validate()
    wait(1000)
    arizona.gamePath()
    arizona.resolution(sampResolutionW or 1920, sampResolutionH or 1080)
    arizona.focus(true)
    arizona.sendClientId()
    arizona.cef.send("onActiveViewChanged|null")
    -- onSvelteAppInit is NOT sent here: the real launcher sends it AFTER the login DialogResponse
    -- (capture: validation → onActiveViewChanged|null → DialogResponse → onSvelteAppInit). The user
    -- script sends it from its login handler once the password is answered.
end

--------------------------------------------------------------------------------
-- 220/17 executeEvent: ack the server's CEF poke and dispatch `cef.on` handlers.
--------------------------------------------------------------------------------
-- Layout: [220][17][u32 0][u16 payloadLen][u8 encoded][payload][u32 requestId]. The requestId MUST be
-- parsed FORWARD, accounting for the u8 `encoded` flag between the length and the text — otherwise it
-- is read one byte early, the heartbeat ack echoes a corrupt id, and the server's watchdog kills the
-- session with "ОШИБКА 7721". The ack mirrors the real client: [220][17][u32 0][u32 requestId].
local function onCefPoke(id: number, bs)
    if id ~= AZ then
        return
    end
    local total = bs:getNumberOfUnreadBytes()
    if total < 12 then
        return
    end
    bs:setReadOffset(8) -- skip the [220] packet-id byte
    if bs:readUInt8() ~= SUB.executeEvent then
        return
    end
    bs:setReadOffset(6 * 8) -- the u16 payload length, after [220][17][u32 0]
    local payloadLen = bs:readUInt16()
    if total < 9 + payloadLen + 4 then
        return
    end
    -- One "encoded" flag byte (0 = raw) sits between the length and the text (Arizona's
    -- [u16 len][u8 encoded][text] maybeEncoded format). It MUST be consumed here: skipping it reads
    -- the requestId one byte early, so the heartbeat ack echoes a corrupt id and the server's
    -- watchdog kills the session with "ОШИБКА 7721".
    bs:readUInt8()

    -- Only read the JS payload when someone is listening; otherwise skip straight to the requestId.
    local hasHandlers = next(cefHandlers) ~= nil
    local js = ""
    if hasHandlers and payloadLen > 0 then
        js = bs:readString(payloadLen)
    else
        bs:setReadOffset((9 + payloadLen) * 8)
    end
    local requestId = bs:readUInt32()

    packet(AZ, SUB.executeEvent):u32(0):u32(requestId):sendEx(1, 9, 2)

    if hasHandlers then
        -- window.executeEvent('event.name', 'json args');
        local name, args = js:match("executeEvent%('([^']*)',%s*'(.-)'%)")
        if name and cefHandlers[name] then
            cefHandlers[name](args or "", requestId)
        end
    end
end

registerHandler("onReceivePacket", onCefPoke)

--------------------------------------------------------------------------------
-- Suppress the driver's automatic RequestClass(128).
--------------------------------------------------------------------------------
-- Arizona has no class-selection flow: a live capture shows the real client sends RequestClass zero
-- times in an entire session (the server drives spectate → login → SetSpawnInfo → spawn on its own),
-- so the driver's one automatic send is a bot fingerprint. Dropping it is safe: the driver arms its
-- self-spawn fallback from the server's TogglePlayerSpectating(on), not the class response.
local RPC_REQUEST_CLASS = 128
local function dropRequestClass(id: number, _bs): boolean?
    if id == RPC_REQUEST_CLASS then
        return false
    end
    return nil
end

registerHandler("onSendRPC", dropRequestClass)

--------------------------------------------------------------------------------
-- Post-join entity-streamer init RPC (252).
--------------------------------------------------------------------------------
-- The real launcher sends this 8-byte RPC 252 right after ClientJoin (before the login dialog) — a
-- capture shows `OUT id=252 len=8` at that point, reversed body `03 00 03 01 01 B2 32 00`. Our bot
-- had stopped sending it (lost in a refactor), and the server appears to withhold login-acceptance /
-- SetSpawnInfo without it. Call from the user script's `onInitGame`. Body is a fixed constant.
function arizona.streamerInit()
    sampSendRpc(252, string.char(0x03, 0x00, 0x03, 0x01, 0x01, 0xB2, 0x32, 0x00))
end

return arizona
