--!nonstrict
-- Arizona launcher emulation — USER-SIDE implementation.
--
-- The reusable plumbing lives in bundled modules: `packet` (the chainable byte builder) and
-- `arizona` (the abstract CEF/validation packet shapes). Everything launcher-specific lives HERE:
-- the auth key, the client/app/join versions, and the `onSendClientJoin` handler that wires them.
-- Copy this file and change the four constants below to emulate a different launcher.

require("addon")
local sampev = require("samp.events")
local arizona = require("arizona")

local AUTH_KEY = "263083C359F5AE44AD3AFC8551F4208E6C84A36F6CC"
local CLIENT_VERSION = "Arizona PC"
local APP_VERSION = "onSvelteAppVersion|1.0.0|ea94b29b"
local JOIN_VERSION = 4057

function sampev.onSendClientJoin(version, mod, nickname, challengeResponse, joinAuthKey, clientVer, challengeResponse2)
    newTask(arizona.validation, 100)
    arizona.sendCef("onSvelteAppInit")
    arizona.sendCef(APP_VERSION)
    return { JOIN_VERSION, 1, nickname, challengeResponse, AUTH_KEY, CLIENT_VERSION, challengeResponse2 }
end
