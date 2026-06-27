--!nonstrict
-- Bundled Arizona packet mechanics — ABSTRACT.
--
-- This module carries the wire SHAPES of the Arizona CEF / validation packets, built on the generic
-- `packet` builder. It holds no identity: the auth key, the client/app/join versions and the
-- `onSendClientJoin` handler all live in the USER script that requires this (see
-- example_scripts/arizona_launcher_emulation.luau). Change those there, not here.

require("addon") -- newTask/wait scheduler used by validation()
local packet = require("packet")

local AZ = 220 -- Arizona CEF / validation packet id

local arizona = {}

-- 220/18 CEF/Svelte message: [220][18][u16 len][text][u32 0][u16 0].
function arizona.sendCef(text: string)
    packet(AZ, 18):string16(text):u32(0):u16(0):send()
end

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

-- 220/140 fabricated game path: [220][140][u32 len][path][0]. Nick comes from the host getter.
function arizona.gamePath()
    local path = string.format(
        '"gta_sa.exe"  -c -h  -p 7777 -n %s -mem 2048 -window -x -widescreen -graphics -enable_grass -arizona -userId %s -cdn 0,0,0 -referrer',
        getBotNick(), arizona.uniqueUserId()
    )
    packet(AZ, 140):u32(#path):text(path):u8(0):send()
end

-- The post-join validation set: 220/20 resolution, 220/50 focus, 220/38 client id, 220/140 path.
-- Resolution is read from the host globals; waits ~1s first, like the reference launcher.
function arizona.validation()
    wait(1000)
    packet(AZ, 20):u32(sampResolutionW or 1920):u32(sampResolutionH or 1080):send()
    packet(AZ, 50):u8(1):send()
    packet(AZ, 38):text(arizona.uniqueUserId()):send()
    arizona.gamePath()
end

return arizona
