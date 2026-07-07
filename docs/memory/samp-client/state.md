# samp-client/state.rs

## Module (LocalPlayer model)
<anchor: module>

The high-level local-player model, shared between the driver (authoritative sync) and the script
engine's `getBot*`/`setBot*` bindings.

[`LocalPlayer`] composes typed sub-states — on-foot, in-vehicle, aim/camera and weapon inventory
— plus identity/world fields and the driver-control flags. The driver builds outgoing sync from
it and folds incoming RPCs back into it; native features (e.g. client emulation) read and write the
same model. Wire (de)serialisation stays in `samp-proto`; this is the in-memory view above it.

---
