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
        '"gta_sa.exe"  -c -h %s -p 7777 -n %s -mem 2048 -window -modern_scale -widescreen -trees_new -enable_grass -arizona -userId %s -cdn 1,1,1 -referrer ',
        host or arizona.host,
        getBotNick(),
        arizona.userId
    )
    packet(AZ, SUB.commandLine):u16(#path):text(path):sendEx(1, 9, 2)
end

-- The full post-join validation set, in the real client's order: command line, resolution, focus,
-- client id, then the CEF `onSvelteAppInit`. Waits briefly first like the launcher.
function arizona.validate()
    wait(200)
    arizona.gamePath()
    arizona.resolution(sampResolutionW or 1920, sampResolutionH or 1080)
    arizona.focus(true)
    arizona.sendClientId()
    arizona.cef.send("onSvelteAppInit")
end

--------------------------------------------------------------------------------
-- 220/17 executeEvent: ack the server's CEF poke and dispatch `cef.on` handlers.
--------------------------------------------------------------------------------
-- Layout: [220][17][u32 0][u16 payloadLen][payload][u32 requestId][optional trailing byte]. The
-- requestId MUST be parsed FORWARD from payloadLen — the binary executeEvent variants carry a trailing
-- byte after the id, so reading "the last 4 bytes" echoes a corrupt id and the server's watchdog kills
-- the session with "ОШИБКА 7721". The ack mirrors the real client: [220][17][u32 0][u32 requestId].
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
    if total < 8 + payloadLen + 4 then
        return
    end

    -- Only read the JS payload when someone is listening; otherwise skip straight to the requestId.
    local hasHandlers = next(cefHandlers) ~= nil
    local js = ""
    if hasHandlers and payloadLen > 0 then
        js = bs:readString(payloadLen)
    else
        bs:setReadOffset((8 + payloadLen) * 8)
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

return arizona
