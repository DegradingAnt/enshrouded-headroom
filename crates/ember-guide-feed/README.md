# Ember Guide Feed

W45 supplies a small background scheduler for ember-blueprint's existing guide
session. It adds no source format, converter, world coordinates or native API.

A notify 8.2.1 parent-directory watch triggers a single-slot wake channel.
The callback only filters events, sets flags and tries to send a wake token.
One worker performs bounded reads, hashing, existing parsing/conversion and
Session polling. A fixed debounce coalesces writes without extending forever.
Recovery reads also run at a bounded cadence, so missed events and temporary
missing files can recover. Parent-watch errors are retried on that worker.
Access events are ignored to prevent source reads from triggering themselves.
Before each bounded source check, an existing watcher unregisters and registers
its parent again through notify. Windows notify 8 can silently detach a deleted
parent without an error callback, so owning a watcher alone is insufficient.
An already absent registration is expected; other renewal failures deny Current
and are retried. Successful registration is required before a check can become
Current, including after an active parent is deleted and recreated. The same
backend is reused; no second watcher/backend or unbounded retry loop is added.
The application supplies a trusted local regular file; filesystem I/O can block.

Each publication is an immutable Arc. `SourceSnapshot::Current` is successful
as of its last completed check. `Unavailable` has no accepted geometry. `Stale`
retains the last model with an explicit failure; `current()` returns None for
stale/unavailable/stopped publications. A consumer must hide retained geometry
or label it stale. Previously held Arcs describe past observations, never live
freshness. A source event observed during conversion prevents that attempt from
being published as current, and schedules another check.

Highcrown sources use the existing raw-plan converter initially and
Session::poll_highcrown afterwards. Byte changes receive a local sequence even
when display version 3.0 is unchanged. Generic blueprint files use parse and
Session::poll_interchange; their producer must advance the existing sequence
and content hash correctly. Identity, frame and revision conflicts use existing
Session validation. No rejected geometry is installed.

An optional previous Session is checked against the configured resource limits
before spawning. It preserves revision continuity, selected stable IDs,
placement, and all fidelity/omission metadata. The worker reuses reconciliation
for valid replacements and marks removed selected IDs visibly removed. Persist
and restore the existing session/model state if restart continuity matters.
This slice adds no input-command channel or controller/selection binding.

Default limits come from ember-blueprint: 16 MiB of raw input, 10,000 objects,
100,000 primitives and 500,000 points. This accepts a 6.26 MB raw plan. Callers
may tighten these limits but cannot raise them. Event backlog is one token.
Only one read/conversion runs at a time. Default debounce is 100 ms and recovery
interval is 2 s; configuration enforces 25 ms to 1 s debounce and a recovery
interval between debounce and 60 s. This bounds bytes and scheduling work,
not the time taken by a blocked filesystem. Publish/read locks hold only an Arc
pointer swap/clone; parsing and hashing never hold that lock. Data-only Reader
clones can outlive the owner and observe its final Stopped publication. There are no
consumer callbacks to outlive the service.

Call start, stop and Drop on ordinary application workers. Stop wakes and joins
this service worker, then its final Stopped publication remains fixed. Drop
calls stop. Neither stop nor Drop is permitted in DllMain, under a native loader
lock, or in game/render/present/input callbacks. Stop can wait for local I/O or
upstream watch setup. Explicit stop reports an unexpected worker panic.

Notify 8's Windows Drop sends an asynchronous Stop request. Joining our worker
and freezing publications DOES NOT prove termination of upstream watcher
threads. The hosting code/module must remain loaded through backend quiescence;
this crate provides no safe dynamic DLL-unload boundary. The initial SDK probe
host must be process-resident. A late upstream callback sees the stop flag and
returns without queueing work or publishing data, but its code still needs a
loaded module. Release all readers and snapshot Arcs before any later host-approved
module unload. Native renderer availability remains false; camera, world epoch,
picking, world placement and PlayStation controls remain separate work.

## Dependency and licence receipt

notify is pinned to `=8.2.1` with default features disabled. Its maintained native
parent-directory backend handles platform events and replacement behavior; this
crate does not implement a filesystem watcher. The latest stable upstream
release was 2026-10-25, checked 2026-10-29. Version 9 was still a prerelease.
No additional queue, async runtime, watcher backend or debouncer is added.

- [notify 8.2.1 release](https://github.com/notify-rs/notify/releases/tag/notify-8.2.1)
- [Upstream manifest](https://github.com/notify-rs/notify/blob/notify-8.2.1/notify/Cargo.toml)
- [notify CC0-1.0 licence](https://github.com/notify-rs/notify/blob/notify-8.2.1/notify/LICENSE-CC0)
- [Documented editor/parent-directory behavior](https://docs.rs/notify/latest/notify/)
- [Windows asynchronous Drop implementation](https://github.com/notify-rs/notify/blob/notify-8.2.1/notify/src/windows.rs)

The resolved notify-types 2.2.0 is MIT OR Apache-2.0, with both texts shipped in
its registry source. Necessary shared Windows dependencies are walkdir 2.5.0,
same-file 1.0.6 and winapi-util 0.1.11 (MIT alternatives); existing bitflags,
libc, log and windows-sys are reused. Other-platform dependencies are recorded
in Cargo.lock but are not enabled as extra runtime features on Windows.
The maintainer grants an exact notify 8.2.1 CC0-1.0 policy exception for W45;
this does not broaden the repository's general licence allowlist.

## Validation

Run the focused suite from the workspace root:

~~~text
cargo test -p ember-guide-feed --all-targets --locked
cargo test -p ember-guide-feed --doc --locked
cargo clippy -p ember-guide-feed --all-targets --locked -- -D warnings
cargo fmt -p ember-guide-feed --check
~~~

Tests use private temporary files and actual upstream notify, covering initial
loading, same-version byte edits, retained selection/fidelity, malformed and
oversized input, delete/recreate, atomic replacement and rename, burst writes,
identity rejection/recovery, missing-parent recovery, active-parent deletion/
recreation followed by a native-event-only edit, and stop/frozen output.
The source guide, game, server, site and renderer are not modified or exercised.
