--!nonstrict
-- Arizona login — USER-SIDE implementation.
--
-- The Rust core no longer answers the server's login dialog; this script does. It watches for the
-- Arizona authorisation dialog and replies with the account password via `RPC_DialogResponse`. Copy
-- this file, set ACCOUNT_PASSWORD, and keep your copy private — the password is never printed.
--
-- Dialog text reaches the script as UTF-8 (the host transcodes cp1251 ⇄ UTF-8), so just write the
-- title and password as plain text.

local sampev = require("samp.events")

-- Your Arizona account password (typed into the in-game login dialog).
local ACCOUNT_PASSWORD = "CHANGE_ME"

-- button 1 = the confirm/login button; 65535 = no listbox selection; input = the password.
function sampev.onShowDialog(
    dialogId: number, style: number, title: string,
    button1: string, button2: string, text: string
)
    if title:find("Авторизация") then
        sampSendDialogResponse(dialogId, 1, 65535, ACCOUNT_PASSWORD)
        return false
    end
end
