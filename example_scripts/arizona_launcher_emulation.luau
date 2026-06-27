--!nonstrict
-- Arizona launcher emulation — with our byte-magic syntax sugar.
--
-- Same wire output as arizona_launcher_emulation_classic.luau, but the raw
-- writeInt8(220) soup is replaced by a small typed, chainable packet builder.
-- A packet reads top-to-bottom as the fields it holds, and the magic numbers
-- become meaning: `u32(sampResolutionW)` instead of `128, 7, 0, 0`.

require("addon")
local sampev = require("samp.events")

local AZ = 220 -- Arizona CEF / validation packet id

-- A fresh bitStream wrapped so each writer returns the builder (the userdata
-- methods return nil, so chaining lives here in Lua).
local Packet = {}
Packet.__index = Packet
type Packet = typeof(setmetatable({} :: { bs: any }, Packet))

-- Start a packet with its [id][subId] header.
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

function sampev.onSendClientJoin(version, mod, nickname, challengeResponse, joinAuthKey, clientVer, challengeResponse2)
    newTask(validation, 100)
    sendCef("onSvelteAppInit")
    sendCef("onSvelteAppVersion|1.0.0|ea94b29b")
    return { 4057, 1, nickname, challengeResponse, "263083C359F5AE44AD3AFC8551F4208E6C84A36F6CC", "Arizona PC", challengeResponse2 }
end
