--!nonstrict
-- Bundled packet-construction API: a typed, chainable builder over the native bitStream.
--
-- Server-agnostic — it knows nothing about any protocol. User scripts (and other bundled modules)
-- require it to compose packets readably: `packet(220, 18):string16(text):u32(0):send()`.

local Packet = {}
Packet.__index = Packet
type Packet = typeof(setmetatable({} :: { bs: any }, Packet))

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

function Packet.i8(self: Packet, v: number): Packet
    self.bs:writeInt8(v)
    return self
end

function Packet.i16(self: Packet, v: number): Packet
    self.bs:writeInt16(v)
    return self
end

function Packet.i32(self: Packet, v: number): Packet
    self.bs:writeInt32(v)
    return self
end

function Packet.float(self: Packet, v: number): Packet
    self.bs:writeFloat(v)
    return self
end

function Packet.bool(self: Packet, v: boolean): Packet
    self.bs:writeBool(v)
    return self
end

-- Raw bytes, no length prefix.
function Packet.text(self: Packet, v: string): Packet
    self.bs:writeString(v)
    return self
end

-- Length-prefixed strings (u8 / u16 byte count, then the bytes).
function Packet.string8(self: Packet, v: string): Packet
    self.bs:writeUInt8(#v)
    self.bs:writeString(v)
    return self
end

function Packet.string16(self: Packet, v: string): Packet
    self.bs:writeUInt16(#v)
    self.bs:writeString(v)
    return self
end

-- SA-MP Huffman-encoded string.
function Packet.encoded(self: Packet, v: string): Packet
    self.bs:writeEncoded(v)
    return self
end

function Packet.bytes(self: Packet, list: { number }): Packet
    for _, b in ipairs(list) do
        self.bs:writeUInt8(b)
    end
    return self
end

function Packet.send(self: Packet)
    self.bs:sendPacket()
end

function Packet.sendRpc(self: Packet, id: number)
    self.bs:sendRPC(id)
end

-- Start a packet, writing any leading id bytes as u8 (so `packet(220, 18)` begins a [220][18] packet).
local function packet(...: number): Packet
    local self = setmetatable({ bs = bitStream.new() }, Packet)
    for _, byte in ipairs({ ... }) do
        self.bs:writeUInt8(byte)
    end
    return self
end

return packet
