--!nonstrict
-- Reusable Arizona launcher emulation — lives in the Luau layer, not the Rust core.
--
-- `require('arizona')` installs a `samp.events.onSendClientJoin` handler that rewrites the join to
-- the Arizona variant and emits the CEF init + post-join validation packets a real Arizona client
-- sends. Other servers don't need any of this, which is exactly why it lives here.
--
-- Packets are built with a small typed, chainable builder, so each one reads as the fields it holds
-- (`u32(sampResolutionW)` instead of the magic bytes `128, 7, 0, 0`).

require("addon")
local sampev = require("samp.events")

local AZ = 220 -- Arizona CEF / validation packet id
local AUTH_KEY = "263083C359F5AE44AD3AFC8551F4208E6C84A36F6CC"
local CLIENT_VERSION = "Arizona PC"
local JOIN_VERSION = 4057

local Packet = {}
Packet.__index = Packet
type Packet = typeof(setmetatable({} :: { bs: any }, Packet))

-- Start a packet with its [220][subId] header.
local function packet(subId: number): Packet
    local self = setmetatable({ bs = bitStream.new() }, Packet)
    self.bs:writeUInt8(AZ)
    self.bs:writeUInt8(subId)
    return self
end

function Packet.u8(self: Packet, v: number): Packet
    self.bs:writeUInt8(v)
    return self
end

function Packet.u16(self: Packet, v: number): Packet
    self.bs:writeUInt16(v)
    return self
end

function Packet.u32(self: Packet, v: number): Packet
    self.bs:writeUInt32(v)
    return self
end

function Packet.text(self: Packet, v: string): Packet
    self.bs:writeString(v)
    return self
end

-- A CEF string field: u16 length prefix, then the bytes.
function Packet.cefString(self: Packet, v: string): Packet
    self.bs:writeUInt16(#v)
    self.bs:writeString(v)
    return self
end

function Packet.send(self: Packet)
    self.bs:sendPacket()
end

local function sendCef(message: string)
    packet(18):cefString(message):u32(0):u16(0):send()
end

local function uniqueUserId(): string
    local chars = "0123456789abcdef"
    local id = table.create(64)
    for i = 1, 64 do
        local n = math.random(1, #chars)
        id[i] = string.sub(chars, n, n)
    end
    return table.concat(id)
end

local function sendGamePath()
    local path = string.format(
        '"gta_sa.exe"  -c -h  -p 7777 -n %s -mem 2048 -window -x -widescreen -graphics -enable_grass -arizona -userId %s -cdn 0,0,0 -referrer',
        getBotNick(), uniqueUserId()
    )
    packet(140):u32(#path):text(path):u8(0):send()
end

local function validation()
    wait(1000)
    packet(20):u32(sampResolutionW or 1920):u32(sampResolutionH or 1080):send() -- resolution
    packet(50):u8(1):send()                                                     -- window focused
    packet(38):text(uniqueUserId()):send()                                      -- client id
    sendGamePath()
end

local arizona = {}

-- Register the join rewrite + CEF/validation. Called automatically on require; safe to call again.
function arizona.install()
    function sampev.onSendClientJoin(version, mod, nickname, challengeResponse, joinAuthKey, clientVer, challengeResponse2)
        newTask(validation, 100)
        sendCef("onSvelteAppInit")
        sendCef("onSvelteAppVersion|1.0.0|ea94b29b")
        return { JOIN_VERSION, 1, nickname, challengeResponse, AUTH_KEY, CLIENT_VERSION, challengeResponse2 }
    end
end

arizona.install()
return arizona
