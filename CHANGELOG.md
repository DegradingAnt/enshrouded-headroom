# Changelog

Notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.1.0] - unreleased

These notes cover implemented work on `feat/v1`, not a released artifact. The runtime governor, NVIDIA Reflex, AMD Anti-Lag and startup scales are implemented. In-game and hardware verification remain separate release gates.

### Added

#### Core and diagnostics

- A Windows x64 ASI plugin with settings, startup logging and guarded Rust callbacks.
- Executable hash checks, including the Steam build `f68140a3-steam`. Unknown builds leave code patches off by default.

#### Rendering and latency

- A texture-streaming budget based on device-local GPU memory, with a reserve setting and conservative handling of integrated GPUs.
- A checked async-compute startup patch that respects the game setting and command-line overrides.
- Optional AboveNormal process priority after the first renderer device is created.
- Vulkan device/present observation, a frame-time ring and monitor refresh-rate detection.
- Scene-settings and terrain hooks for distance controls. The grass redirect keeps the game's 90 m limit until capacity headroom is proven.
- Optional HDR paper white from 40 to 400 nits and opt-in HDR peak correction with a game or explicit display target.
- A 50 ms FPS governor with a default 75 fps target, display-refresh handling, live VRAM checks, world-signal holds, debug forcing and periodic status reports.
- NVIDIA Reflex runtime pacing with supported-driver capability checks, present markers, bounded waits, lifecycle protection and background debug timing reports.
- Experimental AMD Anti-Lag runtime for supported AMD devices, with Reflex precedence, matched INPUT/PRESENT markers and quiet slow-call/device-loss stops; not verified on AMD hardware.
- Checked startup application of High decoration, distant-object and terrain-ring scales, leaving original values unchanged at their 1.0 defaults.

#### Development and packaging

- An `emberkit` shared library for native guards, logging, scanning, patching, build identity, settings primitives and hook queues, with root API adapters.
- A file-independent schema for all 35 read settings; the default INI retains 31 keys and omits four developer settings.
- Client-only Headroom startup with dedicated-server rejection; the shared server build identity is maintainer-supplied evidence.
- Workspace CI checks for formatting, Clippy, documentation, tests and the root ASI build.
- Mutation-test coverage and bounded settings-parser property tests.
- Dependency advisory and licence checks, complete third-party notices and hash-checked packaging of the plugin and loader.
- A generated settings reference covering every shipped default, startup-log version agreement checks, an author release checklist and an unpublished Nexus draft.

### Changed

- Settings and logging can fall back to `%LOCALAPPDATA%\enshrouded-headroom\` when the plugin folder cannot be used.
- Invalid settings produce a warning and a documented default or clamped value. Decimal commas are accepted for numeric settings.
- The debug-only scatter probe is off by default. Its hook records atomic observations; a background worker samples counters once every 500 ms.

### Fixed

- Packaging refuses an archived ASI that differs from the built source before replacing the final zip.

- Existing INI files are preserved when creating defaults.
- Shared PE section parsing validates malformed headers and section bounds.
- Checked writes preserve mismatching bytes and report read-back or rollback failures.
- Logs identify the settings file actually selected and explain unknown-build restrictions.

[0.1.0]: https://github.com/DegradingAnt/enshrouded-headroom/tree/feat/v1
