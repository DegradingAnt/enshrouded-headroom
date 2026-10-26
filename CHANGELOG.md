# Changelog

Notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

These notes cover merged work on `feat/v1`. The runtime governor thread, NVIDIA Reflex calls and AMD Anti-Lag calls are still in progress.

### Added

- A Windows x64 ASI plugin with settings, startup logging and guarded Rust callbacks.
- Executable hash checks, including the Steam build `f68140a3-steam`. Unknown builds leave code patches off by default.
- A texture-streaming budget based on device-local GPU memory, with a reserve setting and conservative handling of integrated GPUs.
- A checked async-compute startup patch that respects the game setting and command-line overrides.
- Optional AboveNormal process priority after the first renderer device is created.
- Vulkan device/present observation, a frame-time ring and monitor refresh-rate detection.
- Scene-settings and terrain hooks for distance controls. The grass redirect keeps the game's 90 m limit until capacity headroom is proven.
- Optional HDR paper white from 40 to 400 nits and opt-in HDR peak correction with a game or explicit display target.
- FPS governor decision logic and NVIDIA Reflex/AMD Anti-Lag mode selection, ready for their separate runtime integration.
- Windows CI checks for formatting, Clippy, documentation, tests and the ASI build.
- Mutation-test coverage and bounded settings-parser property tests.
- Dependency advisory and licence checks, complete third-party notices and hash-checked packaging of the plugin and loader.

### Changed

- Settings and logging can fall back to `%LOCALAPPDATA%\enshrouded-headroom\` when the plugin folder cannot be used.
- Invalid settings produce a warning and a documented default or clamped value. Decimal commas are accepted for numeric settings.
- The scatter counter probe is off by default and limits sampling to once every 500 ms.

### Fixed

- Existing INI files are preserved when creating defaults.
- Shared PE section parsing validates malformed headers and section bounds.
- Checked writes preserve mismatching bytes and report read-back or rollback failures.
- Logs identify the settings file actually selected and explain unknown-build restrictions.

[Unreleased]: https://github.com/DegradingAnt/enshrouded-headroom/tree/feat/v1
