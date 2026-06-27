--!nonstrict
-- This file is part of the SAMP.Lua project.
-- Licensed under the MIT License.
-- Copyright (c) 2016, FYP @ BlastHack Team <blast.hk>
-- https://github.com/THE-FYP/SAMP.Lua

local utils = {}

function utils.decompress_health_and_armor(hpAp: number): (number, number)
	local hp = math.min(bit32.rshift(hpAp, 4) * 7, 100)
	local armor = math.min(bit32.band(hpAp, 0x0F) * 7, 100)
	return hp, armor
end

function utils.compress_health_and_armor(health: number, armor: number): number
	local hp = health >= 100 and 0xF0 or bit32.lshift(health / 7, 4)
	local ap = armor >= 100 and 0x0F or bit32.band(armor / 7, 0x0F)
	return bit32.bor(hp, ap)
end

-- TODO: ffi sync structs — Luau has no ffi. The original allocated a C struct
-- from 'samp.synchronization' and memcpy'd the raw bytes via
-- bs:readBuffer/writeBuffer/getDataPtr (none of which the host exposes yet).
-- These are stubbed: reads return an empty table, writes are no-ops. Replace
-- with field-by-field bitStream IO (mirroring handlers.lua) once needed.

function utils.create_sync_data(st: string)
	-- TODO: ffi sync — return a plain table placeholder
	return {}
end

function utils.read_sync_data(bs, st: string)
	-- TODO: ffi sync — host lacks bs:readBuffer
	return {}
end

function utils.write_sync_data(bs, st: string, ffiobj)
	-- TODO: ffi sync — host lacks bs:writeBuffer; no-op
end

function utils.process_outcoming_sync_data(bs, st: string)
	-- TODO: ffi sync — host lacks bs:getDataPtr; yield an empty placeholder arg
	return {{}}
end

return utils
