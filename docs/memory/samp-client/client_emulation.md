# samp-client/client_emulation.rs

## Module (standard-client emulation)
<anchor: module>

Standard client behaviours a real SA-MP client always provides and a bare bot otherwise omits:
answering the server's `ClientCheck` queries, streaming a weapon inventory, reporting a plausible
aim/camera and periodic score-ping activity, and keeping vehicle ownership consistent. Without
them a server treats the connection as malformed and drops it. Ported from a community MoonLoader
client script (its auth/join handling is excluded; that lives in the Luau Arizona launcher).

Only the timing/bookkeeping that is not player state lives here; the weapon inventory, aim and
vehicle live in [`LocalPlayer`]. The driver consults this at the incoming-RPC and outgoing-sync
seams and sends whatever it returns.

---
