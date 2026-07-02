--!nonstrict
-- Arizona launcher emulation — USER-SIDE implementation.
--
-- The reusable plumbing lives in bundled modules: `packet` (the chainable byte builder) and
-- `arizona` (the abstract CEF/validation packet shapes). Everything launcher-specific lives HERE:
-- the client/app/join versions, the server host, and the `onSendClientJoin` handler. The gpci/auth
-- key is RANDOM by default: the driver's `generate_gpci` hands a fresh valid `×1001` token in as
-- `joinAuthKey` (the server only enforces that format), and the bundled `arizona` module randomizes
-- the 220/140 `-userId` and 220/38 client-id per run — so there is nothing machine-specific to capture.

require("addon")
local sampev = require("samp.events")
local arizona = require("arizona")

local CLIENT_VERSION = "Arizona PC"
-- Frontend build hash — rotates with launcher updates (re-derive: search the launcher's
-- frontend.zip main.bundle.js for cef_sendClientMessage('onSvelteAppVersion', ...)).
local APP_VERSION = "onSvelteAppVersion|1021|5a6d6aa2"
local SERVER_HOST = "bumblebee.arizona-rp.com"
local JOIN_VERSION = 4057

function sampev.onSendClientJoin(
    version: number, mod: number, nickname: string, challengeResponse: number,
    joinAuthKey: string, clientVer: string, challengeResponse2: number
)
    arizona.host = SERVER_HOST
    -- Pre-join CEF, in the real client's order.
    arizona.cef.send(APP_VERSION)
    arizona.cef.send("inventoryItemsLoaded")
    -- Post-join validation set (220/140, 20, 50, 38, onSvelteAppInit) fires shortly after.
    newTask(arizona.validate, 100)
    return { JOIN_VERSION, 1, nickname, challengeResponse, joinAuthKey, CLIENT_VERSION, challengeResponse2 }
end

-- Once the server accepts the join, send the entity-streamer init RPC 252 the real launcher sends
-- before login (the server withholds login-acceptance / SetSpawnInfo without it).
function sampev.onInitGame()
    arizona.streamerInit()
end
