-- RPC Capture (pcap) — MoonLoader capture that writes a libpcap file our RakClient dissector reads.
--
-- Unlike the text rpc_capture.log, this reproduces RakClient's `--pcap` byte layout, so a real-client
-- capture flows through `dissect` / `objects` / `rpcscan` exactly like a bot capture:
--   * each SA-MP message → one synthetic IPv4+UDP datagram (LINKTYPE_RAW), client 10.13.37.1 ↔ server;
--   * UDP payload = a single-message RakNet reliability datagram (Unreliable), the shape
--     `dissect_datagram` parses: bit(0 acks) u16(msgNum) bits(4: wire=6 Unreliable) bit(0 split)
--     compressed-u16(dataBits) align payload;
--   * an RPC payload is `[20 ID_RPC][rpcId][compressed-u32 bitLen][args]` (`wire::parse_rpc`);
--     a raw packet payload is `[id][body]` as-is;
--   * OUTBOUND datagrams are byte-ciphered with the server port (the client encrypts; the dissector
--     decrypts by the UDP dst port). INBOUND stays plaintext.
-- The exact recipe is guarded by crates/raknet/tests/mlpcap_framing.rs.
--
-- Install: copy to the game's `moonloader/` dir. Output: `rpc_capture.pcap` in that dir.

script_name("RPC Capture pcap")
script_author("claude")
script_version("1.0")

require("sampfuncs")
require("samp.events") -- installs the RakNet hook chain + raknet bitstream API

local CLIENT_IP = {10, 13, 37, 1} -- RakClient's synthetic client addr (raknet::pcap CLIENT_ADDR)
local CLIENT_PORT = 1337
local ID_RPC = 20
local RELIABILITY_WIRE_UNRELIABLE = 6 -- discriminant(0) + RELIABILITY_WIRE_BASE(6)

-- Port-keyed substitution cipher table (raknet::tables::SUBSTITUTION, byte-exact from the binary).
local SUB = {
    0x27, 0x69, 0xFD, 0x87, 0x60, 0x7D, 0x83, 0x02, 0xF2, 0x3F, 0x71, 0x99, 0xA3, 0x7C, 0x1B, 0x9D,
    0x76, 0x30, 0x23, 0x25, 0xC5, 0x82, 0x9B, 0xEB, 0x1E, 0xFA, 0x46, 0x4F, 0x98, 0xC9, 0x37, 0x88,
    0x18, 0xA2, 0x68, 0xD6, 0xD7, 0x22, 0xD1, 0x74, 0x7A, 0x79, 0x2E, 0xD2, 0x6D, 0x48, 0x0F, 0xB1,
    0x62, 0x97, 0xBC, 0x8B, 0x59, 0x7F, 0x29, 0xB6, 0xB9, 0x61, 0xBE, 0xC8, 0xC1, 0xC6, 0x40, 0xEF,
    0x11, 0x6A, 0xA5, 0xC7, 0x3A, 0xF4, 0x4C, 0x13, 0x6C, 0x2B, 0x1C, 0x54, 0x56, 0x55, 0x53, 0xA8,
    0xDC, 0x9C, 0x9A, 0x16, 0xDD, 0xB0, 0xF5, 0x2D, 0xFF, 0xDE, 0x8A, 0x90, 0xFC, 0x95, 0xEC, 0x31,
    0x85, 0xC2, 0x01, 0x06, 0xDB, 0x28, 0xD8, 0xEA, 0xA0, 0xDA, 0x10, 0x0E, 0xF0, 0x2A, 0x6B, 0x21,
    0xF1, 0x86, 0xFB, 0x65, 0xE1, 0x6F, 0xF6, 0x26, 0x33, 0x39, 0xAE, 0xBF, 0xD4, 0xE4, 0xE9, 0x44,
    0x75, 0x3D, 0x63, 0xBD, 0xC0, 0x7B, 0x9E, 0xA6, 0x5C, 0x1F, 0xB2, 0xA4, 0xC4, 0x8D, 0xB3, 0xFE,
    0x8F, 0x19, 0x8C, 0x4D, 0x5E, 0x34, 0xCC, 0xF9, 0xB5, 0xF3, 0xF8, 0xA1, 0x50, 0x04, 0x93, 0x73,
    0xE0, 0xBA, 0xCB, 0x45, 0x35, 0x1A, 0x49, 0x47, 0x6E, 0x2F, 0x51, 0x12, 0xE2, 0x4A, 0x72, 0x05,
    0x66, 0x70, 0xB8, 0xCD, 0x00, 0xE5, 0xBB, 0x24, 0x58, 0xEE, 0xB4, 0x80, 0x81, 0x36, 0xA9, 0x67,
    0x5A, 0x4B, 0xE8, 0xCA, 0xCF, 0x9F, 0xE3, 0xAC, 0xAA, 0x14, 0x5B, 0x5F, 0x0A, 0x3B, 0x77, 0x92,
    0x09, 0x15, 0x4E, 0x94, 0xAD, 0x17, 0x64, 0x52, 0xD3, 0x38, 0x43, 0x0D, 0x0C, 0x07, 0x3C, 0x1D,
    0xAF, 0xED, 0xE7, 0x08, 0xB7, 0x03, 0xE6, 0x8E, 0xAB, 0x91, 0x89, 0x3E, 0x2C, 0x96, 0x42, 0xD9,
    0x78, 0xDF, 0xD0, 0x57, 0x5D, 0x84, 0x41, 0x7E, 0xCE, 0xF7, 0x32, 0xC3, 0xD5, 0x20, 0x0B, 0xA7,
}
local MASK_BYTE = 0xAA

-- Lua 5.1 has no native bit ops; do XOR/AND on bytes arithmetically.
local function bxor(a, b)
    local r, bit = 0, 1
    for _ = 0, 7 do
        local x, y = a % 2, b % 2
        if x ~= y then r = r + bit end
        a, b, bit = math.floor(a / 2), math.floor(b / 2), bit * 2
    end
    return r
end
local function band(a, b)
    local r, bit = 0, 1
    for _ = 0, 7 do
        if a % 2 == 1 and b % 2 == 1 then r = r + bit end
        a, b, bit = math.floor(a / 2), math.floor(b / 2), bit * 2
    end
    return r
end

--------------------------------------------------------------------------------
-- MSB-first bit writer (mirrors samp_proto::BitStreamWriter).
--------------------------------------------------------------------------------
local BitW = {}
BitW.__index = BitW
function BitW.new()
    return setmetatable({ bytes = {}, nbits = 0 }, BitW)
end
function BitW:bit(b)
    local pos = self.nbits % 8
    if pos == 0 then
        self.bytes[#self.bytes + 1] = 0
    end
    if b == 1 or b == true then
        local i = #self.bytes
        self.bytes[i] = self.bytes[i] + 2 ^ (7 - pos) -- pos 0 = MSB
    end
    self.nbits = self.nbits + 1
end
-- The low `count` (<=8) bits of `value`, MSB-first (write_bits_low).
function BitW:bitsLow(value, count)
    for i = count - 1, 0, -1 do
        self:bit(math.floor(value / 2 ^ i) % 2)
    end
end
function BitW:byte(v)
    self:bitsLow(v % 256, 8)
end
function BitW:u16(v)
    self:byte(v % 256) -- little-endian bytes, each MSB-first
    self:byte(math.floor(v / 256) % 256)
end
-- RakNet WriteCompressed for a LE byte array (high zero bytes → one `1` bit each).
function BitW:compressed(le)
    local cur = #le
    while cur > 1 do
        if le[cur] == 0 then
            self:bit(1)
        else
            self:bit(0)
            for i = 1, cur do
                self:byte(le[i])
            end
            return
        end
        cur = cur - 1
    end
    if le[1] < 16 then -- high nibble zero
        self:bit(1)
        self:bitsLow(le[1], 4)
    else
        self:bit(0)
        self:byte(le[1])
    end
end
function BitW:compressedU16(v)
    self:compressed({ v % 256, math.floor(v / 256) % 256 })
end
function BitW:align()
    while self.nbits % 8 ~= 0 do
        self:bit(0)
    end
end
function BitW:writeBytes(str)
    for i = 1, #str do
        self:byte(str:byte(i))
    end
end
function BitW:toString()
    local chars = {}
    for i = 1, #self.bytes do
        chars[i] = string.char(self.bytes[i])
    end
    return table.concat(chars)
end

--------------------------------------------------------------------------------
-- Message → reliability datagram → (cipher) → IPv4/UDP → pcap record.
--------------------------------------------------------------------------------

-- Build the RPC message payload `[20][rpcId][compressed-u32 bitLen][args]` (wire::build_rpc).
local function buildRpc(rpcId, args)
    local w = BitW.new()
    w:byte(ID_RPC)
    w:byte(rpcId)
    local bitLen = #args * 8
    w:compressed({
        bitLen % 256,
        math.floor(bitLen / 256) % 256,
        math.floor(bitLen / 65536) % 256,
        math.floor(bitLen / 16777216) % 256,
    })
    w:writeBytes(args)
    return w:toString()
end

-- Wrap one message payload in a single-message Unreliable datagram (the UDP payload).
local msgCounter = 0
local function frameDatagram(payload)
    msgCounter = (msgCounter + 1) % 65536
    local w = BitW.new()
    w:bit(0) -- no ACKs
    w:u16(msgCounter) -- message number
    w:bitsLow(RELIABILITY_WIRE_UNRELIABLE, 4)
    w:bit(0) -- no split
    w:compressedU16(#payload * 8)
    w:align()
    w:writeBytes(payload)
    return w:toString()
end

-- Byte cipher (raknet::cipher::encrypt): out[0]=checksum XOR(b&0xAA); out[1+i]=SUB[b], odd i XOR key.
local function encrypt(str, port)
    local key = bxor(port % 256, 0xCC)
    local checksum = 0
    for i = 1, #str do
        checksum = bxor(checksum, band(str:byte(i), MASK_BYTE))
    end
    local out = { string.char(checksum) }
    for i = 1, #str do
        local v = SUB[str:byte(i) + 1]
        if (i - 1) % 2 == 1 then
            v = bxor(v, key)
        end
        out[#out + 1] = string.char(v)
    end
    return table.concat(out)
end

local function le32(v)
    return string.char(v % 256, math.floor(v / 256) % 256, math.floor(v / 65536) % 256,
        math.floor(v / 16777216) % 256)
end
local function be16(v)
    return string.char(math.floor(v / 256) % 256, v % 256)
end

local logFile
local serverIp = { 127, 0, 0, 1 }
local serverPort = 7777

-- Write one message to the pcap: frame → (cipher if outbound) → IPv4+UDP → record.
local function writeRecord(outbound, payload)
    local datagram = frameDatagram(payload)
    local udpPayload = outbound and encrypt(datagram, serverPort) or datagram

    local srcIp, dstIp, srcPort, dstPort
    if outbound then
        srcIp, srcPort, dstIp, dstPort = CLIENT_IP, CLIENT_PORT, serverIp, serverPort
    else
        srcIp, srcPort, dstIp, dstPort = serverIp, serverPort, CLIENT_IP, CLIENT_PORT
    end

    local udpLen = 8 + #udpPayload
    local ipTotal = 20 + udpLen
    local ip = table.concat({
        string.char(0x45, 0x00), be16(ipTotal), be16(0), be16(0x4000),
        string.char(64, 17), be16(0), -- TTL, proto UDP, header checksum 0
        string.char(srcIp[1], srcIp[2], srcIp[3], srcIp[4]),
        string.char(dstIp[1], dstIp[2], dstIp[3], dstIp[4]),
        be16(srcPort), be16(dstPort), be16(udpLen), be16(0), -- UDP header (checksum 0)
        udpPayload,
    })

    local sec = os.time()
    local usec = math.floor((os.clock() % 1) * 1e6)
    logFile:write(le32(sec) .. le32(usec) .. le32(#ip) .. le32(#ip) .. ip)
    logFile:flush()
end

-- Raw bytes of a bitstream (from offset 0), restoring the read cursor.
local function rawBytes(bs)
    if bs == 0 then
        return ""
    end
    local saved = raknetBitStreamGetReadOffset(bs)
    local ok, nbytes = pcall(raknetBitStreamGetNumberOfBytesUsed, bs)
    if not ok or not nbytes or nbytes <= 0 then
        return ""
    end
    raknetBitStreamSetReadOffset(bs, 0)
    local ok2, raw = pcall(raknetBitStreamReadString, bs, nbytes)
    raknetBitStreamSetReadOffset(bs, saved)
    return (ok2 and raw) or ""
end

addEventHandler("onSendRpc", function(id, bs)
    writeRecord(true, buildRpc(id, rawBytes(bs)))
end)
addEventHandler("onReceiveRpc", function(id, bs)
    writeRecord(false, buildRpc(id, rawBytes(bs)))
end)
addEventHandler("onSendPacket", function(id, bs)
    writeRecord(true, rawBytes(bs))
end)
addEventHandler("onReceivePacket", function(id, bs)
    writeRecord(false, rawBytes(bs))
end)

function main()
    while not isSampfuncsLoaded() or not isSampAvailable() do
        wait(0)
    end
    -- Resolve the real server ip:port for the synthetic framing + cipher key.
    local addr, port = sampGetCurrentServerAddress()
    if addr then
        local a, b, c, d = addr:match("(%d+)%.(%d+)%.(%d+)%.(%d+)")
        if a then
            serverIp = { tonumber(a), tonumber(b), tonumber(c), tonumber(d) }
        end
    end
    if port and port > 0 then
        serverPort = port
    end

    logFile = io.open(getWorkingDirectory() .. "\\rpc_capture.pcap", "wb")
    -- libpcap global header, little-endian (matches raknet::pcap::create):
    -- magic 0xA1B2C3D4, version 2.4, thiszone 0, sigfigs 0, snaplen 65535, LINKTYPE_RAW 101.
    logFile:write(le32(0xA1B2C3D4))
    logFile:write(string.char(2, 0, 4, 0)) -- version_major=2, version_minor=4 (u16 LE each)
    logFile:write(le32(0)) -- thiszone
    logFile:write(le32(0)) -- sigfigs
    logFile:write(le32(65535)) -- snaplen
    logFile:write(le32(101)) -- LINKTYPE_RAW
    logFile:flush()

    sampAddChatMessage(("[RPC Capture pcap] -> rpc_capture.pcap (server %d.%d.%d.%d:%d)"):format(
        serverIp[1], serverIp[2], serverIp[3], serverIp[4], serverPort), 0xFF44FF44)
    wait(-1)
end

function onScriptTerminate(scr)
    if scr == thisScript() and logFile then
        logFile:close()
    end
end
