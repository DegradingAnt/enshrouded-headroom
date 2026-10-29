# ember-measurement

A pure Rust measurement and input-planning core. It reuses `ember-blueprint`'s canonical `Frame` and `Geometry::Polyline`. It has no ASI export, game-memory reader, camera, ray caster, renderer, key polling or controller mapping. It adds no registry dependency.

The live item-free ruler remains incomplete. W42 must separately prove native units, world/origin epochs, picking, camera/projection, render ownership and lifecycle. A future integration also needs in-game keyboard and PlayStation controller acceptance checks. Supplying canonical observations to this library asserts a caller contract; it does not prove that native integration.

## Length and geometry contracts

`Point` accepts finite canonical [X east, Y up, Z south] metre coordinates within +/-1,000,000 on each axis. `measure` requires W38's explicit canonical frame. It reports signed B-A displacement, nonnegative XYZ lengths, and a robust 3D diagonal. Reversed picks and negative coordinates are valid.

| Report | Unit |
| --- | --- |
| XYZ and diagonal | Metres |
| Small spans, including diagonal | Length / 0.5 m |
| Large X/Z spans | Length / 4 m |
| Large Y spans | Length / 2 m |

Fractional spans remain fractional. There is no orientation-independent large-piece diagonal count. These lengths do not imply inventory or build-piece counts.

Two distinct points define world-aligned bounds. A full box yields 12 unique nonzero polylines. A flat rectangle yields four; a line yields one; a coincident pair yields none. The library invents no thickness for degenerate bounds. Outlines are measurement references, not solid placements.

`inclusive_cells` operates only on explicit integer `CellAddress` values and checks abs(B-A)+1 on each axis. A 2 m face-to-face small-grid separation is four spans. Explicit cell addresses 0 through 4 count five cells. Equal points span zero metres; equal cell addresses count one cell. Raw face picks and snapped lattice points do not establish cell-address semantics.

## Snap calibration

`Snap::Off` is the default. Requested small-grid snapping requires a `GridOrigin` that the caller explicitly calibrated and confirmed for the matching `WorldEpoch`. There is no implicit zero origin or fallback to unsnapped geometry. A missing, future, stale or mismatched origin rejects that new preview/pick while preserving accepted pins.

Session snapping also checks when the origin was confirmed. Initial calibration must be confirmed at or after the first context activation. A known world/epoch transition or recovery after unavailable context requires confirmation strictly after its activation receipt. Equal milliseconds cannot prove that reconfirmation followed that event. A still-age-valid old origin cannot survive a world round trip. The standalone `snap_to_small` calculation has no session history; its caller owns any activation barrier.

The lattice is origin + round((point-origin)/0.5)*0.5. Exact halfway ties round away from zero in relative lattice coordinates, including negative positions. Snapped coordinates must still satisfy the point bound. Accepted pins keep their coordinates; changing the preview's snap request does not transform existing pins.

## Observation and ownership contracts

A session starts inactive and unavailable. `WorldEpoch` separates an opaque world identity from its explicit coordinate/origin lifetime. Unknown world, unknown epoch and absent native binding have explicit unavailable variants. Numeric epoch zero is valid only when explicitly supplied as known.

After any reported loss of world knowledge, a recovery observation must have been observed strictly after that loss receipt. A larger sequence attached to an older or equal timestamp cannot restore pins. Rejected current-world observations keep output unavailable and establish a new confirmation boundary without deleting dormant pins. Clock regression records this boundary at the monotonic high-water mark. Repeated unavailable view queries do not move it. Fresh confirmation still requires an explicit allowed Begin to reacquire capture.

Every aim observation, including hit, miss and unavailable data, must also pass the activation boundary used for calibration. After a context transition or recovery, its observation time must be strictly after activation. Matching context identity, increasing sequence and acceptable age cannot admit an old queued observation. Rejection clears the transient candidate and reports unavailable aim while preserving accepted pins. This check also applies with Snap::Off. Fresh legitimate misses still clear previews without moving pins.

World and aim streams each maintain their own strictly increasing `Stamp::sequence` across world changes. Their observation times may stay equal but cannot decrease. Callers supply monotonic milliseconds for every operation. The age policy is explicit and bounded; the default 250 ms is a pure-core policy, not a claim about native cadence. Future observations, regressing clocks, expired samples and unrepresentable deadlines fail closed. Rejected observations never preserve an earlier transient preview.

Each fresh ordered known world/epoch transition clears pins, preview and capture, even if its canonical frame is unsupported. Reusing an epoch with a changed mapping is rejected. An older epoch cannot replace the currently known epoch of the same world. Unknown/stale context retains pins only as hidden context-tagged data; fresh confirmation of the identical context plus an allowed `Begin` can restore them. A different known context resets them. The producer must never relabel an old context as a fresh different-world observation.

`Begin`, `Pick`, `Clear` and `Exit` are abstract actions. Every action, aim receipt and view checks current focus, known closed native menus and an explicit exclusive measurement input grant. A conflict rejects the action without moving pins, suspends the mode, and requires a new allowed `Begin`. The future adapter must release native capture whenever `Session::mode()` is not `Mode::Active`. This core neither claims native input nor emulates buttons.

The first accepted Pick pins A. The second pins B. A third accepted Pick starts a new A and removes B. Each pick consumes its current aim candidate. Clear removes both pins and the transient aim. Exit releases the mode and hides its dormant pins. A legitimate aim miss clears the preview and prevents a pick; it preserves accepted pins in the same current world.

`Session::view` rechecks world freshness and input permission on every call, even without new producer events. `ViewStatus` labels a pinned pair and a transient preview separately. A pinned pair does not expire merely because its original aim hit aged. A preview additionally requires a fresh aim and, when requested, a fresh origin. Unavailable views expose no world context, coordinates or outline.

Call `view` for every render frame using current adapter assertions. A view's inclusive deadline is only an upper freshness bound. A later menu, focus, world or epoch event invalidates a cached view immediately; a cached draw list is not proof that it is still current. No view may be used after its deadline or carried into another context.

## Offline validation

The integration tests use synthetic observations only inside the test target. They cover bounded arithmetic, axis-specific units, unique box edges and degeneracy, inclusive-count overflow, origin-relative snapping, sample ordering and freshness, dormant pins, world transitions and input-conflict combinations. The crate's doctest demonstrates offline geometry only.

Run focused checks from the workspace:

```text
cargo fmt -p ember-measurement --check
cargo test --locked --offline -p ember-measurement --all-targets
cargo test --locked --offline -p ember-measurement --doc
cargo clippy --locked --offline -p ember-measurement --all-targets --all-features -- -D warnings
cargo doc --locked --offline -p ember-measurement --no-deps --document-private-items
```

Passing these checks proves the pure calculation and state contracts. It supplies no native calibration, rendering, keyboard/gamepad or multiplayer runtime evidence. The core never crafts items or mutates a game world, save, server or inventory.
