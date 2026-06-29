--!nonstrict
-- Arizona "type 3" anti-cheat attestation responder (inbound RPC 186 → outbound RPC 187).
--
-- The server periodically challenges the client to prove it is a genuine, unmodified install by
-- HMAC-ing chosen byte regions of `core.asi`'s embedded `RT_RCDATA` resources. Reversed from
-- `core.asi` (`custom_packet_protocol_handler`) and byte-verified against live captures (see memory
-- `arizona-type3-attestation`). The crypto primitives (`sha256`/`hmacSha256`) and the three resource
-- blobs (`arizonaCoreResource`) are host-provided; this module owns the protocol. Requiring it
-- registers the handler, so a script gets attestation just by `require("arizona")`.
--
-- Challenge (RPC id stripped): [u8 3][16 key][16 nonce][u8 n] n×{u8 type,u8 flag,u32 off,u16 size}[u8 trailerFlags]
-- Response (RPC 187):          [u8 3][16 nonce][32 HMAC][u16 trailerLen][trailer]
--   HMAC = HMAC_SHA256(key, nonce ‖ SHA256(region_1) ‖ … ‖ SHA256(region_n) ‖ trailer)

local packet = require("packet")

local TYPE3 = 3 -- the type-3 marker byte that opens both challenge and response
local RPC_CHALLENGE = 186
local RPC_RESPONSE = 187
local FIELD_TYPE_RESOURCE = 4 -- only static-resource fields are statically answerable

-- A "clean" attestation trailer: the anti-debug words are zero (no debugger) and the 0x40 counter is
-- zero. The trailer is fed to the HMAC and echoed verbatim, so it always self-verifies; the server
-- inspects it only for a clean signal. Bit order matches `core.asi`'s `sub_1001F970`.
local function buildTrailer(flags: number): string
    local t = ""
    for _, bit in ipairs({ 0x04, 0x08, 0x10 }) do
        if bit32.band(flags, bit) ~= 0 then
            t ..= string.rep("\0", 4)
        end
    end
    if bit32.band(flags, 0x40) ~= 0 then
        t ..= string.rep("\0", 2)
    end
    return t
end

-- Parse one challenge off `bs` and build the RPC 187 response packet, or return nil to decline:
-- malformed, or a field we can't answer statically (a live-memory read, or an unknown/out-of-range
-- resource). The caller sends nothing on a decline rather than risk a wrong answer.
local function buildResponse(bs)
    bs:setReadOffset(0) -- earlier onReceiveRPC handlers may have advanced the shared stream
    if bs:readUInt8() ~= TYPE3 then
        return nil
    end
    local key = bs:readString(16)
    local nonce = bs:readString(16)
    if #key ~= 16 or #nonce ~= 16 then
        return nil
    end
    local n = bs:readUInt8()
    if n > 16 then
        return nil
    end
    local hashes = ""
    for _ = 1, n do
        local ftype = bs:readUInt8()
        local flag = bs:readUInt8()
        local offset = bs:readUInt32()
        local size = bs:readUInt16()
        if ftype ~= FIELD_TYPE_RESOURCE then
            return nil -- live-memory (type 0/1) reads are not statically answerable
        end
        local res = arizonaCoreResource(flag)
        if not res or offset + size > #res then
            return nil
        end
        hashes ..= sha256(string.sub(res, offset + 1, offset + size))
    end

    local trailer = buildTrailer(bs:readUInt8())
    local mac = hmacSha256(key, nonce .. hashes .. trailer)

    local out = packet():u8(TYPE3):text(nonce):text(mac):u16(#trailer)
    if #trailer > 0 then
        out:text(trailer)
    end
    return out
end

registerHandler("onReceiveRPC", function(id: number, bs)
    if id ~= RPC_CHALLENGE then
        return
    end
    local response = buildResponse(bs)
    if response then
        response:sendRpc(RPC_RESPONSE)
    end
end)

return {}
