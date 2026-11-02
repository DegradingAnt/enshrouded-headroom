# Ember Building

The first pure data/state slice of [W40](https://github.com/DegradingAnt/enshrouded-headroom/issues/73).
This is not a playable Building Tools mod. W40 remains open.

The crate has no native client or server adapter, transport, authenticated game
session mapping, native world/grid calibration, UI, PlayStation controls,
hologram, rendering or runtime hooks. The only shipped native adapter is
UnavailableAdapter. It always rejects before payment or world changes.

## Command contract

A trusted host supplies a stable session key and an increasing connection
generation through Building::connect. The host must obtain these from its
independently authenticated connection mapping. The returned Session is an
opaque lookup handle; constructing the core or calling connect does not prove
authentication. Client command payloads contain no actor, entity, native pointer,
price, inventory or authority field. The host must never decode a client-selected
entity or identity into the trusted binding.

Each generation starts at sequence 1. Accepted commands must be contiguous.
Replays, sequence gaps, stale handles, wrong-owner selection IDs and stale world
epochs reject without changing state or sequence. Invalid actions also leave the
sequence unchanged; a future transport must acknowledge the returned outcome
and current snapshot, rather than count every received payload as accepted.
InputContext comes from the host's control layer separately from the command.
Menu, focus and conflicting controls return Ignored without changing anything.
There are no hardcoded keys, F8 binding, crafted item or controller emulation.

The one mode selects two corners of an inclusive full cuboid. Every grid cell
appears once, with X varying fastest, then Y, then Z. Confirm requires the exact
current selection ID. Clear and cancel abandon all selection state, including a
confirmed candidate. Selecting another corner while selected or confirmed
rejects; the caller must clear or cancel first.

GridPoint contains integer lattice indices and a host-declared world epoch.
These indices have no assumed metres, game block size, origin or axis mapping.
They are distinct from W38's existing canonical metre geometry. This crate does
not duplicate W39 placement math or project grid points into a hologram. A later
verified native/overlay adapter must establish the actual coordinate binding.

Disconnect clears corners, confirmed work and sequence state while retaining
the highest generation. Reconnect requires a higher generation. A monotonically
increasing world epoch disconnects all live bindings and clears their selections.
Old generations and selections cannot revive. Generation tombstones remain in
the bounded table; they are not evicted to make room for an unknown identity.

| Resource | Default | Hard maximum |
| --- | --- | --- |
| Tracked session identities, including tombstones | 16 | 64 |
| Inclusive cells per candidate | 1024 | 4096 |
| Accepted commands per connection generation | 4096 | 1,000,000 |
| Absolute grid index | 1,000,000 | 1,000,000 |

Limits are positive and validated. The table, command state, faults and cell
iteration are bounded. Candidate construction checks the full cuboid before
publication and does not allocate one stored object per cell. Admission is
serialized through a mutable Building reference. Transport scheduling, payload
parsing, request-rate limits and persistent recovery remain host responsibilities.

## Native boundary remains unavailable

Building::execute accepts only the owning live caller's stored confirmed
selection. A copied BuildPlan, snapshot or selection ID cannot directly commit
anything. Execution consumes the stored candidate before invoking an adapter.

NativeAdapter is a trusted system boundary for mock contract tests. Its opaque
DryRun and Payment values are consumed in order: full native dry run, normal
native payment, then native commit. The core has no custom price list, creative
cost path, direct commit method, refund or automatic retry. The two-units-per-cell
price in tests is only a mock fixture; it has no claimed relationship to game
materials or costs.

A future native implementation must recover the authenticated actor before any
selection/cost mutation and preserve that actor, world, complete area and exact
native placement/payment record through the entire pipeline. It must independently
prove the authoritative server ABI/build, transport, original game thread, grid
binding, inventory/base permissions, native dry run, normal payment and complete
commit outcome. It must preserve native undo bookkeeping. Each callback must
revalidate any native context that can change between stages. The NativeProof
names, returned handles and mock successes are not evidence that these game
facts exist. Future native client/server targets must use emberkit for native
infrastructure and callback panic guards. This standard-library-only core does
not need a native dependency.

A dry-run rejection consumes the candidate without attempting payment. By
contract, failed dry runs must not mutate anything. Payment or commit failure,
an incomplete reported commit count, or a callback unwind leaves a bounded
reconciliation fault. Clear, cancel, reconnect and world changes cannot remove
that fault or retry the operation. Even a reported cost refusal is conservatively
latched because this core cannot prove whether native inventory changed. No
reconciliation API is provided in this slice. A future host must reconcile the
actual native outcome before recovery; replacing the core to evade a fault would
violate this integration contract. Successful adapter reports are assertions at
this boundary, not independently verified runtime receipts.

The full owned multiplayer mod, native-style UI, PlayStation input and hologram
integration still require separate implementation and runtime validation. Replace,
refunds, free build, global chests and other modes remain outside this slice.

## Focused validation

~~~text
cargo fmt -p ember-building --check
cargo clippy --offline --locked -p ember-building --all-targets --all-features -j 2 -- -D warnings
cargo test --offline --locked -p ember-building --all-targets -j 2
cargo test --offline --locked -p ember-building --doc -j 2
cargo doc --offline --locked -p ember-building --no-deps -j 2
~~~

Tests exercise two-player interleaving, ownership, replay/sequence bounds,
disconnect/reconnect, epoch changes, context conflicts, cancel/clear, resource
bounds, normal-cost ordering, native prerequisite/permission refusal, partial
payment/commit outcomes and prevention of retries or direct commit bypass.
They do not establish game or server behavior.
