require("addon")
local sampev = require("samp.events")

function sendcef(text)
	local bs = bitStream.new()
	bs:writeInt8(220)
	bs:writeInt8(18)
	bs:writeInt16(#text)
	bs:writeString(text)
	bs:writeInt32(0)
	bs:writeInt16(0)
	bs:sendPacket()
	bs:reset()
end

function sampev.onSendClientJoin(version, mod, nickname, challengeResponse, joinAuthKey, clientVer, challengeResponse2)
		newTask(Validation, 100)
		sendcef('onSvelteAppInit')
		sendcef('onSvelteAppVersion|1.0.0|ea94b29b')
		return {4057, 1, nickname, challengeResponse, "263083C359F5AE44AD3AFC8551F4208E6C84A36F6CC", "Arizona PC", challengeResponse2}
end

function getUniqueUserId()
	local chars = "0123456789abcdef"
    local userId = ""
    
    for i = 1, 64 do
        local randomIndex = math.random(1, #chars)
        userId = userId .. string.sub(chars, randomIndex, randomIndex)
    end
    return userId
end

function sendGamePath()
    local server_ip, port = getServerAddress():match("(.*):(.*)")
    local player_name = getBotNick()
    local game_path = string.format("\"gta_sa.exe\"  -c -h  -p 7777 -n %s -mem 2048 -window -x -widescreen -graphics -enable_grass -arizona -userId %s -cdn 0,0,0 -referrer", player_name, getUniqueUserId())
    print("Fake game path: " .. game_path)
    local bs = bitStream.new()
    bs:writeInt8(220)
    bs:writeInt8(140)
    bs:writeInt32(#game_path)
    bs:writeString(game_path)
    bs:writeInt8(0)
    bs:sendPacket()
    bs:reset()
end

function Validation()
	wait(1000)
	sendPacket(220, 20, 128, 7, 0, 0, 56, 4, 0, 0)
    sendPacket(220, 50, 1)
	sendPacket(220, 38, 53, 52, 50, 57, 98, 101, 51, 48, 56, 102, 51, 100, 98, 97, 57, 52, 97, 100, 99, 52, 55, 57, 51, 50, 49, 50, 49, 99, 98, 52, 50, 52, 51, 56, 54, 102, 98, 49, 100, 53, 100, 50, 50, 51, 50, 54, 49, 55, 100, 97, 51, 97, 52, 97, 97, 101, 57, 99, 50, 52, 100, 54, 53, 57)
    sendGamePath()
end

function sendPacket(...)
	local bs = bitStream.new()
	local bytes = {...}
	for _, byte in ipairs(bytes) do
		bs:writeInt8(byte)
	end
	bs:sendPacket()
	bs:reset()
end