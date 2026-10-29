# Ruler position calibration probe

A separate client-only ASI for the first native walking-ruler calibration. It does not activate the measuring session or draw a UI. No world epoch or calibrated metre value is supplied to another mod. Holograms remain deferred.

The probe is opt-in. Place its own config beside the game executable as `enshrouded-ruler-probe.ini`; `Probe.Enabled=0` is the default. `1` enables one startup attempt on the exact retained client executable. Unknown clients, the server and synthetic host names never enable a hook. Settings take effect after restart. The probe does not edit Headroom, graphics settings, inventory, saves or server files.

When admitted, one simulation-step relay calls the original exactly once. The original runs entirely in C++, outside a Rust panic boundary, and a native exception skips observation. Our Rust observer is separately guarded. A native SEH leaf protects only the read-only component lookup. This separation preserves the game's original call and its exception path even when observation is disabled or faults.

At most one client sample is attempted every 500 ms. The sampler reads only within the callback. It uses that callback's world, verifies the LocalPlayerData hash/index, copies a 56-byte CurrentTransform's first 24 bytes as three signed raw coordinates, and rechecks the local owner. Partial reads, changed owners, unsupported maps, missing data and modified getter entry bytes yield unavailable. The map probe stops after sixteen slots. No native pointer is retained for later dereference, and a world address printed as a diagnostic tag is never an epoch.

The normal callback path does no logging, formatting, file I/O, allocation or waiting. A caught Rust panic can report one guard failure. A try-lock publishes one owned observation. One ordinary worker writes `enshrouded-ruler-probe.log`, reports stale input after 1.5 seconds, and stops sampling after ten minutes. Each raw copy is timed; missing timing or a copy over 2 ms stops further sampling and reports unavailable. This is a fuse, not a claim about measured in-game overhead. The DLL is pinned before any hook or worker becomes active. Its relay remains forwarding the original until process exit. This calibration DLL is not reloadable.

The only startup setting is:

| Section | Key | Default | Effect |
|---|---|---|---|
| Probe | Enabled | 0 | Opt into exact-client raw position calibration on next launch |

The exact-target functions, ABI, fields and evidence limits are in `docs/re-notes.md`. The source ABI and map layout derive from MIT EMF at `08d600155fcadc8e45cc9c6f3932a67c717345be`; its required notice is in LICENSE-EMF. No EMF loader, game mutation API, Vulkan layer, backup worker or installed-tool inspection is linked.

Build with `cargo build -p ember-ruler-probe --release`. The resulting `ember_ruler_probe.dll` can be staged as `enshrouded-ruler-probe.asi` after review. Normal workspace validation includes the reader tests and inert C++ driver. The driver checks argument order, exactly-once normal dispatch, preservation of a native exception and the component lookup's SEH fallback. No ignored real-game test is enabled.

Native acceptance remains pending. The controlled test must correlate movements and known spans, exit/rejoin and another world, compare frame-time overhead, and preserve all installed mods. Until that evidence exists, the actual measuring tool stays unavailable.
