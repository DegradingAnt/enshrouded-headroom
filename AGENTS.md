# AGENTS.md: how to work on enshrouded-headroom

Read this file before you change anything. It tells you why the project exists, where things are, and how a change must be made. The task list is in [`docs/codex-worksheet.md`](docs/codex-worksheet.md).

## Why this project exists

enshrouded-headroom is a free performance mod for the PC game Enshrouded (Steam, Vulkan renderer). It is a Rust `.asi` plugin. Ultimate ASI Loader v10.0.0 (`dinput8.dll`) loads it. It does these things:

1. It sizes the game's texture streaming budget to the GPU's VRAM.
2. It turns on async compute.
3. It adds NVIDIA Reflex (planned, Task 16).
4. Its main feature is an fps-target governor (default 75 fps), which is in progress. When there is spare GPU time, the governor raises draw distance: objects first, then grass, then terrain. When frames are late, it takes the raise back. It never goes below the player's menu settings.

The plugin runs inside the game process. A panic or a bad memory write crashes the player's game, so the rules below are strict.

## Where things are

| Path | What |
|---|---|
| `docs/design.md` | The spec. It is the authority. |
| `docs/plan.md` | The task-by-task plan (Tasks 1-18). |
| `docs/re-notes.md` | Reverse-engineering evidence: every game address, signature and struct offset, with its source. |
| `docs/codex-worksheet.md` | Tasks for you, with acceptance criteria. |
| `crates/emberkit/` | Reusable native guard, logging, scan, patch, IAT, build identity, INI primitives/schema, process targeting and MinHook queue. |
| `src/entry.rs` | Start-up order (`InitializeASI` → `entry::start`). |
| `src/config.rs` | The `.ini` parser and defaults (`DEFAULT_INI` must match `enshrouded-headroom.default.ini`). |
| `src/guard.rs` | The panic guard. Every extern function body runs inside `guard(...)`. |
| `src/scan.rs`, `src/sigs.rs` | Byte-signature scanner and the signature table. |
| `src/patch.rs`, `src/iat.rs` | Checked memory writes and import-table hooks. |
| `src/features/` | Texture budget, async compute (and later features). |
| `src/vk/` | The Vulkan `GetProcAddress` wrapper chain and the device/swapchain/present hooks. |
| `src/frame_timer.rs` | The lock-free frame-time ring that the present hook writes. |
| `src/display.rs` | The game window's monitor refresh rate. |
| `src/build_id.rs` | The known game builds (exe SHA-256). An unknown build turns code patches off. |
| `tests/` | Integration tests: `startup_logging.rs`, and `vk_chain.rs` (a fake `vulkan-1.dll` driven through the real import slot). |
| `examples/asi_host.rs` | A small host that loads the built `.asi` (used for smoke tests). |

New mods are new crates under `crates/` and depend on `emberkit`.

## How to build and test

The crate is **Windows-only** (`x86_64-pc-windows-msvc`, toolchain pinned in `rust-toolchain.toml`).

- On Windows:
  - `cargo fmt --check`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test`
  - `cargo asi` (builds `target/release/enshrouded-headroom.asi`)
- On Linux you cannot run the tests. Do this instead:
  - `rustup target add x86_64-pc-windows-msvc`
  - `cargo check --target x86_64-pc-windows-msvc --all-targets`
  - `cargo clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings`
  - Say in the PR that the tests were not run, so the Windows CI runs them.
- Tests marked `#[ignore]` need the real game exe (`ENSHROUDED_EXE`, `ENSHROUDED_STEAM_EXE`) or a desktop. Do not remove the `#[ignore]` and do not make them pass without the file.

## Rules for code

1. **No panics in non-test code:**
   - no `unwrap`, `expect`, slice indexing (`x[i]`), unchecked arithmetic that can overflow, or `panic!`;
   - use `get(..)`, `checked_*`, `saturating_*` and `Option`/`Result`.
2. **Every extern function** (`extern "system"`, `extern "C"`) wraps its whole body in `guard(Feature::…, fallback, body)`.
3. **Every `unsafe` block** has a `// SAFETY:` comment that is true. Every `unsafe fn` has a `# Safety` doc section.
4. **Never invent a game address, offset or byte pattern.** Use only values in `docs/re-notes.md` or `src/sigs.rs`. If you need a new one, stop and say so in the PR.
5. **Hooks stay small.** The present path (`wrap_present`) only records a timestamp and calls the real function. Do not add work there.
6. **TDD.** Write the failing test first, then the code. A test must fail if the behaviour it names is removed.
7. **Match the existing style:**
   - doc comments on every item;
   - short, plain sentences;
   - names as in the surrounding code;
   - no new dependency without a reason in the PR.
8. **Settings:**
   - a new `.ini` key goes into `DEFAULT_INI` and `enshrouded-headroom.default.ini` (a test checks that they match);
   - it goes into the settings table in `docs/design.md` section 8;
   - a bad value warns and falls back to its default.

## Rules for docs

- This repo is public. Refer to the person who made decisions as **"the author"**.
- Do not add personal names, local paths of the author's machine, or private notes.
- Write in plain, short sentences: one idea per sentence, active voice.

## Rules for git and pull requests

- Work on a **new branch from `feat/v1`**. Open the PR **into `feat/v1`**, never into `main`.
- **Never push to `feat/v1` or `main` directly.** Never force-push.
- One worksheet task per PR. Put the task id (for example `W3`) in the PR title.
- Commit messages: `type(scope): what changed`, for example `feat(priority): …`, `fix(entry): …`, `docs: …`.
- The maintainer reviews every PR before it is merged. A PR that breaks a rule above is sent back.

## Do not touch: work in progress elsewhere

- `src/governor/` and the module list in `src/lib.rs`: the governor is being built (Task 11).
- Plan Tasks 12-16 (scene-settings hook, draw-distance knobs, start-up scales, the governor thread and Reflex). They need reverse engineering of the game exe, which is not in this repo.
- Anything that needs the game exe or an in-game test.

If a task in the worksheet seems to need one of these, stop and say so in the PR instead.
