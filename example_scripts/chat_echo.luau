-- Demo script: echo incoming chat / server messages, and reply "pong" to "!ping".

function sampev.onChatMessage(playerId: number, text: string)
    print("[chat]", playerId, text)
    if text == "!ping" then
        sampSendChat("pong")
    end
end

function sampev.onServerMessage(color: number, text: string)
    print("[server]", text)
end
