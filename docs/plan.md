# enshrouded-headroom Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Rust `.asi` plugin for Ultimate ASI Loader v10.0.0. It raises Enshrouded's texture budget to fit the card's VRAM, turns async compute on, adds NVIDIA Reflex on Vulkan, and runs an fps governor. The governor spends headroom on object, grass and terrain draw distance, and never goes below the player's menu settings.

**Architecture:** One `cdylib` crate, written to `enshrouded-headroom.asi`. `InitializeASI` reads the `.ini`, identifies the game build, applies the start-up patches and installs the hooks. These are a `GetProcAddress` import-slot wrapper chain into Vulkan, plus minhook detours on two game functions. Hooks talk only through mod-owned atomics. A governor thread runs a pure state machine on present-to-present times and writes knob factors back into those atomics.

**Tech Stack:** Rust 1.99.1 (x86_64-pc-windows-msvc), windows-sys 0.61.2, minhook 0.9.0, ash 0.38.0 (types only), configparser 3.3.0, log 0.4.34 + fern 0.7.1.

**Spec:** `docs/design.md` (v2, approved 2026-10-25). Evidence, addresses and signatures: `docs/re-notes.md`. Task 1 copies both into the repo as `docs/design.md` and `docs/re-notes.md`.

## Global Constraints

**Project and build**
- Repo: `C:\Modding\Enshrouded`, a new git repo, MIT licence, public on GitHub only at release (Task 18, with the author's go-ahead).
- Toolchain: pinned in `rust-toolchain.toml` to `1.99.1`, target `x86_64-pc-windows-msvc`.
- Output file: `enshrouded-headroom.asi`. Settings: `enshrouded-headroom.ini`. Log: `enshrouded-headroom.log`. Both sit next to the `.asi` in `<game>\plugins\`, with a fallback to `%LOCALAPPDATA%\enshrouded-headroom\`.
- Profile:
  - `panic = "unwind"` (never `abort`);
  - `lto = "fat"`, `codegen-units = 1`, `opt-level = 3`, `debug = "line-tables-only"`;
  - `+crt-static` through `.cargo/config.toml`.

**Game build**
- Known build `f68140a3`, in two exes with the same game code. Both are known:
  - old install: sha256 `b28b448a8dbf48c932b717f035031f5a89712a7cccfcc2d6cac0d47e6421c12a`, label `f68140a3`;
  - Steam (buildid 26082781): sha256 `d64966e23b1a458ee6f147bcbceac0b577fa7e39a212f371287d8cc119817882`, label `f68140a3-steam`.
- On any other build, code patches and code hooks are OFF unless `AllowUnknownBuild = 1`.

**Rules for game memory and hooks**
- Signatures count only on exactly one match.
- Every patch checks its expected original bytes first. It then calls `VirtualProtect`, writes, reads back, and calls `FlushInstructionCache`.
- Hooks never block, log, allocate or take locks; they touch only atomics.
- The governor thread never reads game memory.
- No wait on a game thread without a timeout. The Reflex semaphore wait is `10 ms`.
- Changed `vkCreateDevice` and `vkCreateSwapchainKHR` calls are retried with the game's original arguments if they fail.
- Every extern function wraps its body in `guard(...)` (Task 3), which runs the original on a panic.

**Defaults** (spec sections 6.4 and 8)
- `[General]`: `TextureBudget=auto`, `TextureReserveGiB=11`, `AsyncCompute=on`, `Reflex=game`, `AllowUnknownBuild=0`. (`TargetFps` is a `[Governor]` key, per spec 6.4 and 8; ruling 2026-10-25.)
- `[Governor]`:

  | Key | Default |
  |---|---|
  | `TargetFps` | 75 |
  | `WindowSeconds` | 2 |
  | `RaiseBelow` | 0.90 |
  | `RaiseHoldSeconds` | 1.0 |
  | `SettleSeconds` | 1.0 |
  | `LowerAbove` | 0.97 |
  | `LowerHoldSeconds` | 0.3 |
  | `HitchFactor` | 2.0 |
  | `BlockSeconds` | 20 |
  | `MinVramFreeMiB` | 1024 |

- `[Knobs]`: each max step `4`; `DecoDistanceScale=1.0`, `DistantObjectsScale=1.0`, `TerrainRingScale=1.0`.

**Knob steps**

| Knob | Steps |
|---|---|
| Object (factor) | `[1.0, 0.85, 0.72, 0.61, 0.52]` |
| Scatter (metres) | `[90.0, 100.0, 110.0, 120.0, 127.0]` |
| Terrain (factor) | `[1.0, 1.25, 1.5, 1.75, 2.0]` |

**Release**
- No game files or third-party binaries are committed. The loader is downloaded and hash-checked by the package script: zip sha256 `3d5f2f2a47f456db878d17c1b135b0eae3209a7d8dabb0ff3f84e1b008c4f3a7`.
- The author writes the disclosure text and reads all code before release.
- Any new non-Rust file starts with a `WHY NOT RUST:` line (project rule).

## Review Focus

1. **A game update** changes the exe hash. The game must start normally. The log names the new hash, says which features are off, and says how to opt in (`AllowUnknownBuild`). Test: `entry::tests::unknown_build_disables_code_features` (Task 6), and the host smoke test in Task 1.
2. **Another tool already patched something.** The 2026-10-23 launcher, an overlay, or another ASI may have changed the texture row, rewritten `GetProcAddress`, or hooked the same function. The mod must detect this, log it, and not double-apply. Tests: `texture_budget::tests::prepare_refuses_changed_row` (Task 7) and `vk::tests::wrap_gpa_calls_saved_pointer` (Task 9).
3. **A hand-edited `.ini`** may contain garbage, a decimal comma (`0,9`), missing sections or out-of-range values. Each bad value falls back to its default or is clamped, and is logged. The plugin never panics on it. Test: `config::tests::bad_values_fall_back_and_warn` (Task 2).
4. **Alt-tab, minimise, a resolution change, or a loading screen** stop presents or re-create the swapchain. The governor makes no decisions while the world is not rendering. Reflex re-arms on a new swapchain, or turns itself off. Tests: `governor::tests::pauses_when_world_not_active` (Task 11) and `reflex::tests::rearm_on_new_swapchain` (Task 16).
5. **Laptops and multi-GPU PCs.** An integrated GPU, or a shared-memory heap reported as device-local, must not get a raised texture budget. Test: `texture_budget::tests::integrated_gpu_unchanged` (Task 7).

---

## File structure

```
C:\Modding\Enshrouded\
  Cargo.toml                      crate `enshrouded-headroom`, cdylib + rlib (rlib so examples/tests link)
  rust-toolchain.toml             channel = "1.99.1"
  .cargo\config.toml              +crt-static; alias `asi`
  LICENSE  README.md  .gitignore
  docs\design.md  docs\re-notes.md  docs\plan.md
  enshrouded-headroom.default.ini the default settings text (also embedded with include_str!)
  src\lib.rs                      mod declarations; InitializeASI and DllMain exports
  src\entry.rs                    start-up sequence
  src\guard.rs                    guard(), panic hook, per-feature kill switches
  src\log.rs                      fern init + fallback directory
  src\config.rs                   Config, parsing, defaults, clamping
  src\build_id.rs                 sha256 (BCrypt) + known-build table
  src\scan.rs                     Pattern, find_unique, Module, section scan
  src\patch.rs                    write_checked, write_u32_checked, alloc_near, rel32
  src\iat.rs                      import-slot lookup and swap
  src\sigs.rs                     every signature and offset (copied from re-notes)
  src\frame_timer.rs              lock-free present ring, QPC helpers
  src\display.rs                  refresh period of the game window's monitor
  src\features\mod.rs
  src\features\texture_budget.rs
  src\features\async_compute.rs
  src\features\startup_scales.rs  7.4 deco/distant + 7.5 voxel rings
  src\knobs\mod.rs                KnobShared atomics, step tables
  src\knobs\object_distance.rs    post-hook on the scene-settings function
  src\knobs\scatter_distance.rs   disp32 redirect + capacity audit/doubling
  src\knobs\terrain_distance.rs   pre-hook on the terrain update function
  src\vk\mod.rs                   wrapper chain
  src\vk\types.rs                 #[repr(C)] structs missing from ash
  src\vram_budget.rs              VK_EXT_memory_budget query
  src\reflex.rs                   VK_NV_low_latency2
  src\governor\mod.rs             pure state machine
  src\governor\stats.rs           p95, quantisation detection
  src\governor_thread.rs          glue: ring + atomics + display + VRAM → governor → atomics
  examples\asi_host.rs            loads an .asi and calls InitializeASI (smoke test)
  scripts\package.ps1             release zip (downloads and verifies the loader)
```

---

### Task 1: Repo, toolchain and a loadable empty plugin

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `.cargo/config.toml`, `.gitignore`, `LICENSE`, `README.md` (stub), `docs/*.md` (copies), `src/lib.rs`, `examples/asi_host.rs`

**Interfaces:**
- Produces: `#[no_mangle] pub extern "C" fn InitializeASI()`. For now it only creates `enshrouded-headroom.log` next to the module and writes the line `enshrouded-headroom <version> loaded`. `DllMain` only calls `DisableThreadLibraryCalls`.
- Produces: `cargo asi`. It is an alias for `rustc --release --lib -- -C link-arg=/OUT:target/release/enshrouded-headroom.asi`.

- [ ] **Step 1:** `rustup update stable` and `rustup install 1.99.1` (the author asked for the latest versions; on 2026-10-25 only `stable` = 1.98.0 was installed). Check: `rustc +1.99.1 --version` prints `1.99.1`. Do not rely on `link.exe` from PATH: rustc finds the MSVC linker itself (VS 2022 Community, MSVC 14.44.35207).
- [ ] **Step 2:** `git init C:\Modding\Enshrouded`. Copy the spec, the re-notes and this plan into `docs/`. Add the MIT `LICENSE` (copyright: DegradingAnt), `.gitignore` (`target/`, `dist/`), and `Cargo.toml`:
  - `[lib] crate-type = ["cdylib", "rlib"]`, name `enshrouded_headroom`;
  - the dependencies at the exact versions in Global Constraints;
  - the release profile from Global Constraints.
- [ ] **Step 3: Write `examples/asi_host.rs`.** It takes an `.asi` path, `LoadLibraryW`s it, calls `InitializeASI`, sleeps 500 ms, and exits 0.
- [ ] **Step 4: Run it.** `cargo asi`, then `cargo run --release --example asi_host -- target/release/enshrouded-headroom.asi`.
  - Expected: exit 0, and `target/release/enshrouded-headroom.log` contains `loaded`.
- [ ] **Step 5: Verify the artefact, not the build log.** `dumpbin` is NOT on PATH on the author's machine (checked 2026-10-25), and Git's own `link.exe` comes first on PATH. So use `python -I -c "import pefile; ..."` (pefile 2024.8.26 is installed). Alternatively use the full path from `vswhere` (`C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.44.35207\bin\Hostx64\x64\dumpbin.exe`). Check that the `InitializeASI` export is present and that `VCRUNTIME140.dll` is absent from the imports.
- [ ] **Step 6:** Commit: `chore: scaffold enshrouded-headroom plugin`.

### Task 2: Settings file

**Files:**
- Create: `src/config.rs`, `enshrouded-headroom.default.ini`

**Interfaces:**
- Produces:
  - `pub struct Config { pub general: General, pub governor: GovernorCfg, pub knobs: KnobsCfg }`
  - `pub enum TextureBudget { Auto, Off, Gib(f64) }`
  - `pub enum AsyncCompute { On, Game }`
  - `pub enum ReflexMode { Game, Off, On, Boost }`
  - `pub fn parse(text: &str) -> (Config, Vec<String>)`. The `Vec` holds warnings.
  - `pub fn read_ini(dir: &Path) -> io::Result<Option<(Config, Vec<String>)>>`. It reads an existing file; `Ok(None)` means it is missing.
  - `pub fn write_default(dir: &Path) -> io::Result<()>`. It creates the default file, or returns `AlreadyExists` if the file exists.
  - `pub const DEFAULT_INI: &str = include_str!("../enshrouded-headroom.default.ini")`
  - `impl Default for Config`, which equals `parse(DEFAULT_INI).0`.
- Field names are the snake_case of the `.ini` keys in spec section 8 (`target_fps: u32`, `raise_below: f64`, `max_object_distance: u8`, …).

Start-up replaced `load_or_create` with `read_ini` and `write_default` through `entry::load_settings`. The loader tries the settings folders in order.

**Clamp ranges**

| Key | Range |
|---|---|
| `TargetFps` | `0` or `30..=360` |
| `WindowSeconds` | `0.5..=10` |
| `RaiseBelow` | `0.5..=0.99` |
| `LowerAbove` | `0.8..=1.0`, and above `RaiseBelow` (otherwise both reset to their defaults) |
| Hold and settle seconds | `0.1..=10` |
| `HitchFactor` | `1.2..=5` |
| `BlockSeconds` | `1..=300` |
| `MinVramFreeMiB` | `0..=8192` |
| `Max*` | `0..=4` |
| `DecoDistanceScale`, `DistantObjectsScale` | `0.5..=1.0` |
| `TerrainRingScale` | `1.0..=1.6` |
| `TextureReserveGiB` | `4..=16` |
| `TextureBudget=<GiB>` | `2..=8` |

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn defaults_match_spec() {
    let (c, w) = parse(DEFAULT_INI);
    assert!(w.is_empty());
    assert_eq!(c.governor.target_fps, 75);
    assert!(matches!(c.general.async_compute, AsyncCompute::On));
    assert!(matches!(c.general.reflex, ReflexMode::Game));
    assert_eq!(c.governor.raise_below, 0.90);
    assert_eq!(c.governor.lower_hold_seconds, 0.3);
    assert_eq!(c.governor.min_vram_free_mib, 1024);
    assert_eq!(c.knobs.max_scatter_distance, 4);
}
#[test] fn bad_values_fall_back_and_warn() {
    let (c, w) = parse("[Governor]\nRaiseBelow=0,85\nLowerAbove=banana\nTargetFps=9999\n[Nope]\nX=1\n");
    assert_eq!(c.governor.raise_below, 0.85);      // decimal comma accepted
    assert_eq!(c.governor.lower_above, 0.97);      // garbage -> default
    assert_eq!(c.governor.target_fps, 360);        // clamped
    assert!(w.len() >= 3);                         // garbage, clamp, unknown section
}
#[test] fn inverted_thresholds_reset_both() {
    let (c, w) = parse("[Governor]\nRaiseBelow=0.98\nLowerAbove=0.9\n");
    assert_eq!((c.governor.raise_below, c.governor.lower_above), (0.90, 0.97));
    assert!(!w.is_empty());
}
#[test] fn missing_file_writes_default() {
    let d = tempdir_in_target("cfg_missing");
    assert_eq!(read_ini(&d).unwrap(), None);
    write_default(&d).unwrap();
    let (c, w) = read_ini(&d).unwrap().unwrap();
    assert_eq!(c, Config::default());
    assert!(w.is_empty());
    assert_eq!(std::fs::read_to_string(d.join("enshrouded-headroom.ini")).unwrap(), DEFAULT_INI);
}
```

  `tempdir_in_target` is a small test helper that creates `target/test-tmp/<name>`, emptied first.
- [ ] **Step 2:** Run `cargo test config::` and expect FAIL.
- [ ] **Step 3:** Implement `parse` with `configparser::ini::Ini::read`. Replace `,` with `.` before parsing a float. Unknown sections and keys produce warnings.
- [ ] **Step 4:** Run `cargo test config::` and expect PASS.
- [ ] **Step 5:** Commit: `feat(config): ini parsing with defaults and clamping`.

### Task 3: Logging and panic guard

**Files:**
- Create: `src/log.rs`, `src/guard.rs`

**Interfaces:**
- Produces:
  - `pub fn log::init(preferred_dir: &Path, level: log::LevelFilter) -> PathBuf`. It returns the log path actually used: `preferred_dir` if writable, otherwise `%LOCALAPPDATA%\enshrouded-headroom\`.
  - `pub enum Feature { TextureBudget, AsyncCompute, StartupScales, ObjectDistance, ScatterDistance, TerrainDistance, VkWrap, Reflex, Governor }`
  - `pub fn guard::is_disabled(f: Feature) -> bool`
  - `pub fn guard::disable(f: Feature, why: &str)`. It logs the reason once.
  - `pub fn guard::guard<T>(f: Feature, fallback: impl FnOnce() -> T, body: impl FnOnce() -> T) -> T`
    - If `f` is disabled, it runs `fallback`.
    - If `body` panics, it calls `disable(f, ...)` and returns `fallback()`.
    - It uses `catch_unwind(AssertUnwindSafe(..))`.
  - `pub fn guard::install_panic_hook()`. The hook logs the message and location instead of printing to stderr.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn guard_returns_body_value() { assert_eq!(guard(Feature::Governor, || 0, || 7), 7); }
#[test] fn guard_catches_panic_and_disables() {
    assert_eq!(guard(Feature::Reflex, || 1, || panic!("boom")), 1);
    assert!(is_disabled(Feature::Reflex));
    assert_eq!(guard(Feature::Reflex, || 2, || 3), 2); // stays disabled
}
#[test] fn log_falls_back_when_dir_unwritable() {
    let p = init(Path::new(r"Z:\definitely\not\writable"), log::LevelFilter::Info);
    assert!(p.starts_with(std::env::var("LOCALAPPDATA").unwrap()));
}
```

- [ ] **Step 2:** Run `cargo test guard:: log::` and expect FAIL.
- [ ] **Step 3:** Implement. The disabled set is a `static DISABLED: AtomicU32` bitmask indexed by `Feature as u32`.
- [ ] **Step 4:** Run the tests and expect PASS. Then run `cargo asi` and the host from Task 1: the log line `loaded` is still written.
- [ ] **Step 5:** Commit: `feat: logging with fallback dir and panic guard`.

### Task 4: Signature scanning

**Files:**
- Create: `src/scan.rs`, `src/sigs.rs`

**Interfaces:**
- Produces:
  - `pub struct Pattern(Vec<Option<u8>>)`
  - `impl Pattern { pub fn parse(s: &str) -> Result<Pattern, ScanError>; pub fn len(&self) -> usize }`. The text is space-separated hex bytes, with `??` as a wildcard.
  - `pub enum ScanError { BadPattern(String), NotFound, Multiple(usize) }`
  - `pub fn find_unique(hay: &[u8], pat: &Pattern) -> Result<usize, ScanError>`
  - `pub struct Module { pub base: usize, pub size: usize }`
  - `pub fn main_module() -> Module`, from the PE `SizeOfImage` of `GetModuleHandleW(null)`.
  - `pub fn find_in_sections(m: &Module, sections: &[&str], pat: &Pattern) -> Result<usize, ScanError>`. It searches the named PE sections in place and returns the VA. Uniqueness counts across all the named sections together.
- Produces `sigs.rs`. Each entry is a `pub const NAME: Sig = Sig { pattern: "...", sections: &[".text"], patch_offset: N }`, holding the exact strings and offsets from the re-notes signature table:
  - `SCENE_SETTINGS_FN` (offset 0)
  - `SCATTER_INSTANCE_LOADS` (distance disp32 at +30)
  - `SCATTER_BRICK_LOAD` (+9)
  - `SCATTER_CAP_CONST` (+12)
  - `SCATTER_CAP_ALLOC` (+12)
  - `DECO_HIGH_IMM` (+9)
  - `DISTANT_HIGH_IMM` (+9)
  - `VOXEL_HIGH_IMMS` (+15, +22, +29 → `patch_offsets: &[15, 22, 29]`)
  - `ASYNC_OVERRIDE` (+3)
  - `TERRAIN_UPDATE_FN` (0)
  - `TEXTURE_TABLE`, which is built from data, see Task 7.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn wildcard_match_once() {
    let hay = [0x90, 0x83, 0xf9, 0x01, 0x75, 0x12, 0xc7, 0x90];
    let p = Pattern::parse("83 f9 01 75 ?? c7").unwrap();
    assert_eq!(find_unique(&hay, &p), Ok(1));
}
#[test] fn two_matches_is_error() {
    let hay = [0xaa, 0xbb, 0xaa, 0xbb];
    assert_eq!(find_unique(&hay, &Pattern::parse("aa bb").unwrap()), Err(ScanError::Multiple(2)));
}
#[test] fn bad_pattern() { assert!(matches!(Pattern::parse("zz"), Err(ScanError::BadPattern(_)))); }
#[test] fn every_sig_parses() { for s in sigs::ALL { Pattern::parse(s.pattern).unwrap(); } }
#[test] fn finds_own_text() {
    // the test binary's own .text contains the prologue of this test fn; just assert the API walks sections
    let m = main_module();
    assert!(m.size > 0x1000);
    assert!(matches!(find_in_sections(&m, &[".text"], &Pattern::parse("de ad be ef 13 37 c0 de 42 42").unwrap()), Err(ScanError::NotFound)));
}
```

- [ ] **Step 2:** Run `cargo test scan::` and expect FAIL.
- [ ] **Step 3:** Implement a naive scan, anchored on the first non-wildcard byte with `memchr`-style `position`. Read the section bounds from the in-memory PE headers.
- [ ] **Step 4:** Run `cargo test scan::` and expect PASS.
- [ ] **Step 5: Cross-check against the real exe.** Write a test-only helper `#[ignore] fn sigs_unique_in_game_exe()`. It reads the game exe from disk (the path in `ENSHROUDED_EXE` if set, else `C:\Program Files (x86)\Steam\steamapps\common\Enshrouded\enshrouded.exe`; a path that is set must be a readable file, or the test panics; only a missing file at the default Steam path skips), maps the sections by file offset, and asserts that every `sigs::ALL` entry is unique. Run `cargo test --lib -- --ignored --nocapture sigs_unique_in_game_exe` and expect PASS. **Run ignored tests with `-- --ignored --nocapture`:** a skip prints a `SKIPPED ...` line and the harness still reports `ok`, and without `--nocapture` the line is hidden, so a green run alone does not show that an exe was read. Look for the real output lines. This is the run recipe for every ignored real-exe test in this plan.
- [ ] **Step 6:** Commit: `feat(scan): exactly-once signature scanning + signature table`.

### Task 5: Patching primitives and import slots

**Files:**
- Create: `src/patch.rs`, `src/iat.rs`

**Interfaces:**
- Produces:
  - `pub struct Patched { pub addr: usize, pub original: Vec<u8> }`
  - `pub enum PatchError { Mismatch { at: usize, found: Vec<u8> }, Protect(u32), ReadBack }`
  - `pub unsafe fn write_checked(addr: usize, expected: &[u8], new: &[u8]) -> Result<Patched, PatchError>`. It makes each region writable (`PAGE_READWRITE` for non-executable regions and `PAGE_EXECUTE_READWRITE` for executable regions), writes, restores each region's own protection, reads back and calls `FlushInstructionCache`.
  - `pub unsafe fn write_u32_checked(addr: usize, expected: u32, new: u32) -> Result<Patched, PatchError>`
  - `pub fn alloc_near(target: usize, size: usize) -> Option<usize>`. It returns a `PAGE_READWRITE` allocation whose whole range is within ±2 GiB of `target`. It walks `VirtualQuery` free regions in 64 KiB steps.
  - `pub fn rel32(next_insn: usize, dest: usize) -> Option<i32>`
- Produces:
  - `pub unsafe fn iat::find_slot(m: &Module, dll: &str, func: &str) -> Option<*mut usize>`. The DLL name match is case-insensitive.
  - `pub unsafe fn iat::swap(slot: *mut usize, new: usize) -> usize`. It returns the old value and uses `write_checked` on the 8 bytes.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn refuses_unexpected_bytes() {
    let mut buf = [1u8, 2, 3, 4];
    let r = unsafe { write_checked(buf.as_mut_ptr() as usize, &[9, 9], &[5, 6]) };
    assert!(matches!(r, Err(PatchError::Mismatch { .. })));
    assert_eq!(buf, [1, 2, 3, 4]);
}
#[test] fn writes_and_keeps_original() {
    let mut buf = [1u8, 2, 3, 4];
    let p = unsafe { write_checked(buf.as_mut_ptr() as usize + 1, &[2, 3], &[7, 8]) }.unwrap();
    assert_eq!(buf, [1, 7, 8, 4]); assert_eq!(p.original, vec![2, 3]);
}
#[test] fn alloc_near_is_in_rel32_range() {
    let here = alloc_near_is_in_rel32_range as usize;
    let a = alloc_near(here, 4096).unwrap();
    assert!(rel32(here, a).is_some() && rel32(here, a + 4095).is_some());
}
#[test] fn finds_own_getprocaddress_slot() {
    let m = crate::scan::main_module();
    let slot = unsafe { iat::find_slot(&m, "kernel32.dll", "GetProcAddress") };
    assert!(slot.is_some());
}
```

  The test binary imports `GetProcAddress` through `windows-sys`. If the linker resolves it through `api-ms-win-core-libraryloader-*`, `find_slot` must also accept that DLL name. Test both names and assert at least one is found.
- [ ] **Step 2:** Run `cargo test patch:: iat::` and expect FAIL.
- [ ] **Step 3:** Implement. `find_slot` walks the import descriptors and matches the name thunk (`IMAGE_IMPORT_BY_NAME`) against the original first thunk.
- [ ] **Step 4:** Run the tests and expect PASS.
- [ ] **Step 5:** Commit: `feat(patch): checked writes, near allocation, import slots`.

### Task 6: Build identity and the start-up skeleton

**Files:**
- Create: `src/build_id.rs`, `src/entry.rs`
- Modify: `src/lib.rs` (`InitializeASI` calls `entry::start()` inside `guard`)

**Interfaces:**
- Produces:
  - `pub fn build_id::sha256_file(path: &Path) -> std::io::Result<[u8; 32]>`, using BCrypt `BCRYPT_SHA256_ALGORITHM` through windows-sys.
  - `pub enum Build { Known(&'static str), Unknown([u8; 32]) }`
  - `pub fn build_id::classify(hash: [u8; 32]) -> Build`
  - `pub const KNOWN: &[(&str, &str)] = &[("b28b448a8dbf48c932b717f035031f5a89712a7cccfcc2d6cac0d47e6421c12a", "f68140a3")]`
  - `pub fn entry::start()`. Order:
    1. config and log;
    2. hash `GetModuleFileNameW(null)` and classify it;
    3. `code_patches_allowed = known || cfg.allow_unknown_build`;
    4. later tasks add their steps here.
  - `pub fn entry::code_patches_allowed() -> bool`, a `OnceLock`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn sha256_of_abc() {
    let p = tempfile_with(b"abc");
    assert_eq!(hex(&sha256_file(&p).unwrap()), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
}
#[test] fn known_build_classified() {
    assert!(matches!(classify(unhex(KNOWN[0].0)), Build::Known("f68140a3")));
}
#[test] fn unknown_build_disables_code_features() {
    assert!(!entry::decide_code_patches(&Build::Unknown([0; 32]), false));
    assert!(entry::decide_code_patches(&Build::Unknown([0; 32]), true));
    assert!(entry::decide_code_patches(&Build::Known("f68140a3"), false));
}
```

  `decide_code_patches(build: &Build, allow_unknown: bool) -> bool` is the pure part of step 3.
- [ ] **Step 2:** Run `cargo test build_id:: entry::` and expect FAIL.
- [ ] **Step 3:** Implement. The log line is `game build: <label|unknown sha256 …>; code patches <on|off>`.
- [ ] **Step 4:** Run the tests and expect PASS. Run the host from Task 1 again. The log must show `unknown` (the host is not the game), `code patches off`, and the process exits 0.
- [ ] **Step 5:** Commit: `feat: build identification and start-up skeleton`.

### Task 7: Texture budget

**Files:**
- Create: `src/features/mod.rs`, `src/features/texture_budget.rs`

**Interfaces:**
- Consumes: `scan::{Module, find_in_sections}`, `patch::write_checked` (the top value is a u64, so 8 bytes), `config::TextureBudget`.
- Produces:
  - `pub const TABLE: [(u64, u64); 5] = [(0, 0x12C00000), (0x11F400000, 0x25800000), (0x180000000, 0x40000000), (0x200000000, 0x5F400000), (0x280000000, 0x80000000)]`
  - `pub fn table_bytes() -> [u8; 80]`, little-endian pairs.
  - `pub fn budget_bytes(device_local: u64, reserve_gib: f64, integrated: bool, mode: &TextureBudget) -> Option<u64>`
    - `None` means "leave unchanged".
    - `Auto`: `clamp(device_local - reserve, 2 GiB, 8 GiB)`, and `None` if the result ≤ 2 GiB or if `integrated`.
    - `Gib(x)`: `x` GiB, clamped 2..=8; `None` if integrated.
    - `Off`: `None`.
  - `pub struct Prepared { top_value_addr: usize }`
  - `pub fn prepare(m: &Module) -> Result<Prepared, TbError>`. It finds `table_bytes()` exactly once in `.rdata` and checks the top value. `TbError::ChangedByOtherTool` if the five thresholds match but the top value ≠ `0x80000000`.
  - `pub unsafe fn apply(p: &Prepared, bytes: u64) -> Result<(), PatchError>`

- [ ] **Step 1: Write the failing tests.** The heap size is the real one from the author's log: `16_753_098_752`.

```rust
const GIB: u64 = 1 << 30;
#[test] fn rtx5080_gets_4_6_gib() {
    assert_eq!(budget_bytes(16_753_098_752, 11.0, false, &TextureBudget::Auto), Some(16_753_098_752 - 11 * GIB));
}
#[test] fn twelve_gb_unchanged() { assert_eq!(budget_bytes(12 * GIB, 11.0, false, &TextureBudget::Auto), None); }
#[test] fn twentyfour_gb_capped_at_8() { assert_eq!(budget_bytes(24 * GIB, 11.0, false, &TextureBudget::Auto), Some(8 * GIB)); }
#[test] fn integrated_gpu_unchanged() { assert_eq!(budget_bytes(32 * GIB, 11.0, true, &TextureBudget::Gib(6.0)), None); }
#[test] fn explicit_gib_clamped() { assert_eq!(budget_bytes(16 * GIB, 11.0, false, &TextureBudget::Gib(20.0)), Some(8 * GIB)); }
#[test] fn prepare_refuses_changed_row() {
    let mut t = table_bytes(); t[72..80].copy_from_slice(&(6 * GIB).to_le_bytes());
    assert!(matches!(prepare_in(&t), Err(TbError::ChangedByOtherTool)));
}
```

  `prepare_in(hay: &[u8]) -> Result<usize, TbError>` is the pure inner part of `prepare`.
- [ ] **Step 2:** Run `cargo test texture_budget::` and expect FAIL.
- [ ] **Step 3:** Implement. In `entry::start`, call `prepare` and store `Prepared` in a `OnceLock`. `apply` is called from `vk::wrap_create_device` (Task 9). This is a data patch, so the spec allows it on unknown builds when the full 80-byte table matches.
- [ ] **Step 4:** Run the tests and expect PASS. Run the ignored test with `cargo test --lib -- --ignored --nocapture texture_table_unique_in_game_exe` (it reads the real exe; see the run recipe in Task 4 Step 5) and expect PASS.
- [ ] **Step 5:** Commit: `feat: VRAM-sized texture budget`.

### Task 8: Async compute

**Files:**
- Create: `src/features/async_compute.rs`

**Interfaces:**
- Consumes: `sigs::ASYNC_OVERRIDE`, `patch::write_checked`, `entry::code_patches_allowed`.
- Produces:
  - `pub const ORIGINAL: [u8; 16] = [0x44,0x38,0xb6,0x89,0x52,0x03,0x00, 0x74,0x0a, 0x0f,0xb6,0x86,0x88,0x52,0x03,0x00]`
  - `pub const NEW: [u8; 16] = [0xb0,0x01, 0x0f,0x1f,0x84,0x00,0x00,0x00,0x00,0x00, 0x66,0x0f,0x1f,0x44,0x00,0x00]`
  - `pub enum Decision { Patch, Skip(&'static str) }`
  - `pub fn decide(cmdline: &str, cfg: &AsyncCompute, code_patches: bool) -> Decision`. Skip reasons: `"command line sets it"`, `"ini: game"`, `"unknown build"`.
  - `pub fn apply(m: &Module) -> Result<(), String>`. It finds the signature, adds `patch_offset` 3 and calls `write_checked(ORIGINAL → NEW)`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn patches_by_default() { assert!(matches!(decide("enshrouded.exe", &AsyncCompute::On, true), Decision::Patch)); }
#[test] fn respects_command_line() {
    assert!(matches!(decide(r#""C:\x\enshrouded.exe" --Disable-Async-Compute"#, &AsyncCompute::On, true), Decision::Skip(_)));
    assert!(matches!(decide("enshrouded.exe --enable-async-compute", &AsyncCompute::On, true), Decision::Skip(_)));
}
#[test] fn respects_ini_and_build() {
    assert!(matches!(decide("x", &AsyncCompute::Game, true), Decision::Skip(_)));
    assert!(matches!(decide("x", &AsyncCompute::On, false), Decision::Skip(_)));
}
#[test] fn new_bytes_decode_as_mov_al_1_and_nops() {
    // NEW is 2 + 8 + 6 bytes; the instruction after it (mov [rbp+0x3e], al) is untouched
    assert_eq!(NEW.len(), ORIGINAL.len());
    assert_eq!(&NEW[..2], &[0xb0, 0x01]);
}
```

- [ ] **Step 2:** Run `cargo test async_compute::` and expect FAIL.
- [ ] **Step 3:** Implement. Match command-line tokens case-insensitively, split on whitespace. Call it from `entry::start` with `GetCommandLineW`.
- [ ] **Step 4:** Run the tests and expect PASS.
- [ ] **Step 5:** Commit: `feat: async compute through the game's own override path`.

### Task 9: Vulkan wrapper chain and frame timer

**Files:**
- Create: `src/vk/mod.rs`, `src/vk/types.rs`, `src/frame_timer.rs`

**Interfaces:**
- Consumes: `iat::{find_slot, swap}`, `features::texture_budget::{apply, budget_bytes}`, `guard`.
- Produces from `frame_timer`:
  - `pub struct Ring` with 4096 `AtomicU64` slots.
  - `pub static FRAMES: Ring`
  - `impl Ring { pub fn push(&self, qpc: u64); pub fn read_since(&self, seq: u64, out: &mut Vec<u64>) -> u64 }`. `read_since` returns the new sequence number.
  - `pub fn qpc_now() -> u64`, `pub fn qpc_to_ms(delta: u64) -> f64`
- Produces from `vk`:
  - `pub fn install(m: &Module) -> Result<(), String>`. It swaps the exe's `GetProcAddress` slot and stores the old value in `static REAL_GPA`.
  - `pub enum Wrap { Gipa, Gdpa, CreateDevice, CreateSwapchain, Present, Submit, Submit2 }`
  - `pub fn classify_gpa(name: &[u8]) -> Option<Wrap>`, `pub fn classify_gipa(name: &[u8]) -> Option<Wrap>`, `pub fn classify_gdpa(name: &[u8], reflex_active: bool) -> Option<Wrap>`. These are pure. `Submit` and `Submit2` are returned only when `reflex_active`.
  - `pub static WRAPPER_ENGAGED: AtomicBool`. It is set when `wrap_gipa` is handed out.
  - `pub fn vk::game_device() -> Option<GameDevice>`, where `GameDevice { device: usize, phys: usize }`. It returns the first device the game creates and its physical device together, or `None` before then.
  - The extern wrappers are `wrap_gpa`, `wrap_gipa`, `wrap_gdpa`, `wrap_create_device`, `wrap_create_swapchain` and `wrap_present`. Each body runs inside `guard(Feature::VkWrap, call_original, …)`.
    - `wrap_gpa` guards: ordinal names (`name as usize <= 0xFFFF`) pass through. Names are compared only when `hModule == GetModuleHandleW("vulkan-1.dll")`.
    - `wrap_create_device` calls `texture_budget::apply` once, on the first call. It reads `vkGetPhysicalDeviceMemoryProperties` (the largest `DEVICE_LOCAL` heap) and `vkGetPhysicalDeviceProperties` (`deviceType == INTEGRATED_GPU`).
    - `wrap_present` pushes `qpc_now()` to `FRAMES`, then delegates to `reflex::on_present` (Task 16), which calls the real present exactly once.
- Produces from `vk/types.rs`: `#[repr(C)]` structs for any `VK_NV_low_latency2` / `VK_KHR_present_id2` types that ash 0.38.0 lacks. Each one gets a test asserting `size_of` against the Vulkan header value.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn classify_names() {
    assert!(matches!(classify_gpa(b"vkGetInstanceProcAddr"), Some(Wrap::Gipa)));
    assert!(classify_gpa(b"vkGetInstanceProcAddrX").is_none());
    assert!(matches!(classify_gipa(b"vkCreateDevice"), Some(Wrap::CreateDevice)));
    assert!(matches!(classify_gdpa(b"vkQueuePresentKHR", false), Some(Wrap::Present)));
    assert!(classify_gdpa(b"vkQueueSubmit", false).is_none());
    assert!(matches!(classify_gdpa(b"vkQueueSubmit2", true), Some(Wrap::Submit2)));
}
#[test] fn ring_keeps_order_and_wraps() {
    let r = Ring::new(); for i in 0..5000u64 { r.push(i); }
    let mut v = Vec::new(); let s = r.read_since(5000 - 10, &mut v);
    assert_eq!(s, 5000); assert_eq!(v, (4990..5000).collect::<Vec<_>>());
}
#[test] fn wrap_gpa_calls_saved_pointer() {
    // install a fake REAL_GPA that records its call; wrap_gpa(kernel32, "Sleep") must route through it
    set_real_gpa_for_test(fake_gpa as usize);
    unsafe { wrap_gpa(GetModuleHandleW(w!("kernel32.dll")), b"Sleep\0".as_ptr()) };
    assert_eq!(FAKE_CALLS.load(SeqCst), 1);
}
#[test] fn wrap_gpa_ordinal_passthrough() {
    set_real_gpa_for_test(fake_gpa as usize);
    unsafe { wrap_gpa(GetModuleHandleW(w!("kernel32.dll")), 5usize as *const u8) }; // must not dereference
    assert_eq!(FAKE_CALLS.load(SeqCst), 2);
}
```

- [ ] **Step 2:** Run `cargo test vk:: frame_timer::` and expect FAIL.
- [ ] **Step 3:** Implement. In `entry::start`, call `vk::install` (this is not a code patch, so it runs on unknown builds too). Add the log lines `vk wrapper installed` and, on first use, `wrapper chain engaged (device created)`.
- [ ] **Step 4:** Run the tests and expect PASS. Run the host from Task 1 and expect exit 0. The host does no Vulkan, so the log says `installed` but not `engaged`.
- [ ] **Step 5:** Commit: `feat(vk): GetProcAddress wrapper chain, device hook, present timer`.

### Task 10: Display period

**Files:**
- Create: `src/display.rs`

**Interfaces:**
- Produces:
  - `pub fn game_window() -> Option<HWND>`. It returns the largest visible top-level window of the current process, using `EnumWindows` and `GetWindowThreadProcessId`.
  - `pub fn refresh_hz(hwnd: HWND) -> Option<u32>`. It uses `MonitorFromWindow`, `GetMonitorInfoW` for the device name, then `EnumDisplaySettingsW(ENUM_CURRENT_SETTINGS).dmDisplayFrequency`. Values ≤ 1 count as `None`.
  - `pub fn period_ms(hz: Option<u32>) -> f64`. It returns `1000/hz`, or `1000/60` when `None`.

- [ ] **Step 1: Write the failing tests:** `assert_eq!(period_ms(Some(120)), 1000.0 / 120.0); assert_eq!(period_ms(None), 1000.0 / 60.0);`. Also `#[ignore] fn primary_monitor_has_rate()`, which asserts `refresh_hz(GetDesktopWindow()).unwrap() >= 30`.
- [ ] **Step 2:** Run `cargo test display::` and expect FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `cargo test display::` and `cargo test -- --ignored primary_monitor_has_rate`, and expect PASS.
- [ ] **Step 5:** Commit: `feat: monitor refresh period for the game window`.

### Task 11: Governor state machine (pure)

**Files:**
- Create: `src/governor/mod.rs`, `src/governor/stats.rs`

**Interfaces:**
- Consumes: `config::GovernorCfg`.
- Produces from `stats.rs`:
  - `pub fn p95(xs: &[f64]) -> f64`
  - `pub fn quantised(xs: &[f64], period_ms: f64) -> bool`. True if ≥ 90 % of `xs` are within ±5 % of `k × period_ms` for some k in 1..=4.
  - `pub fn share_at_or_below(xs: &[f64], limit_ms: f64) -> f64`
- Produces from `governor/mod.rs`:
  - `pub struct KnobSpec { pub enabled: bool, pub max_step: u8 }`
  - `pub struct Inputs<'a> { pub now_s: f64, pub new_frames_ms: &'a [f64], pub world_active: bool, pub period_ms: f64, pub vram_free_mib: Option<u64>, pub baseline_changed: &'a [bool] }`
  - `pub enum Action { Set { knob: usize, step: u8 } }`
  - `pub struct Governor`
  - `impl Governor { pub fn new(cfg: GovernorCfg, knobs: Vec<KnobSpec>) -> Self; pub fn tick(&mut self, i: &Inputs) -> Vec<Action>; pub fn steps(&self) -> &[u8] }`

**Rules** (from spec 6.4; this is the algorithm the implementer must follow):
1. Keep a window of frame times covering the last `window_seconds`.
2. **Effective target:** `target_ms = max(1000/target_fps, period_ms)`. When `target_fps == 0`, never act.
3. **Inactive world:** if `!world_active`, or there was a frame gap over 250 ms in the last 2 s, clear the hold timers and return no actions.
4. **Menu change:** if `baseline_changed[k]`, set knob k to step 0, remove it from the raise stack, and emit `Set{k, 0}`.
5. **Quantised mode**, when `quantised(window, period_ms)`:
   - "headroom" = `share_at_or_below(window, target_ms*1.05) >= 0.95`;
   - "overload" = the share of frames above `target_ms*1.05` is more than 0.02.
6. **Otherwise:**
   - "headroom" = `p95 < raise_below × target_ms`;
   - "overload" = `p95 > lower_above × target_ms`.
7. **Hitch rule:** within `settle + raise_hold` seconds after a raise, any frame over `hitch_factor × target_ms` counts as overload, even if it is over 100 ms. Outside that period, frames over 100 ms are dropped from the window.
8. **Lowering:**
   - when overload has held for `lower_hold_seconds`, and `settle_seconds` have passed since the last change, pop the raise stack (LIFO);
   - set that knob one step down;
   - block that knob's next step for `block_seconds`.
9. **Raising:**
   - when headroom has held for `raise_hold_seconds`, `settle_seconds` have passed, and `vram_free_mib` is `None` or `>= min_vram_free_mib`;
   - raise the first enabled, unblocked knob (in index order) whose step < max by one, and push it on the stack.

- [ ] **Step 1: Write the failing tests.** Each feeds synthetic 50 ms ticks. The test helper `run(g, secs, frame_ms, world_active, period) -> Vec<Action>` feeds `frame_ms` frames until `secs` have passed.

```rust
fn gov() -> Governor { Governor::new(Config::default().governor, vec![KnobSpec{enabled:true,max_step:4}; 3]) }
#[test] fn raises_one_step_per_settle_with_headroom() {
    let mut g = gov(); let a = run(&mut g, 2.05, 10.0, true, 1000.0/240.0);   // 100 fps vs 75 target, 240 Hz
    assert_eq!(g.steps(), &[1, 0, 0]); assert_eq!(a.len(), 1);
}
#[test] fn lowers_after_hold_when_overloaded() {
    let mut g = gov(); run(&mut g, 2.05, 10.0, true, 1000.0/240.0);
    run(&mut g, 3.0, 15.0, true, 1000.0/240.0);                                // 66 fps
    assert_eq!(g.steps(), &[0, 0, 0]);
}
#[test] fn hitch_after_raise_undoes_it() {
    let mut g = gov(); run(&mut g, 2.05, 10.0, true, 1000.0/240.0);
    feed(&mut g, &[10.0, 10.0, 120.0, 10.0]);                                  // 120 ms > 2x13.3 ms, within settle+hold
    run(&mut g, 1.2, 10.0, true, 1000.0/240.0);
    assert_eq!(g.steps(), &[0, 0, 0]);
}
#[test] fn pauses_when_world_not_active() {
    let mut g = gov(); let a = run(&mut g, 5.0, 10.0, false, 1000.0/240.0);
    assert!(a.is_empty()); assert_eq!(g.steps(), &[0, 0, 0]);
}
#[test] fn fifo_120hz_holds_refresh_without_flapping() {
    let mut g = gov(); let p = 1000.0/120.0;
    let a = run_pattern(&mut g, 20.0, &[p, p, p, p, p, p, p, p, p, 2.0*p], true, p); // 10 % at 60 fps
    assert!(a.len() <= 2);                                                       // no flapping
}
#[test] fn target_above_refresh_uses_refresh() {
    let mut g = Governor::new(GovernorCfg{target_fps:144, ..Config::default().governor}, vec![KnobSpec{enabled:true,max_step:4}]);
    run(&mut g, 2.05, 1000.0/120.0, true, 1000.0/120.0);                          // locked at 120 Hz = full headroom
    assert_eq!(g.steps(), &[1]);
}
#[test] fn no_raise_when_vram_low() { /* vram_free_mib Some(512) -> steps stay [0,0,0] */ }
#[test] fn menu_change_resets_knob() { /* raise knob 0 to 1, then baseline_changed=[true,false,false] -> Set{0,0} */ }
#[test] fn runaway_guard_when_frames_do_not_respond() {
    // frame time independent of steps but oscillating across thresholds -> at most 1 raise per BlockSeconds
    let mut g = gov(); let a = run_pattern(&mut g, 60.0, &[10.0; 30].iter().chain([16.0; 30].iter()).copied().collect::<Vec<_>>(), true, 1000.0/240.0);
    let raises = count_raises(&a); assert!(raises <= 60 / 20 + 1);
    assert!(g.steps().iter().all(|&s| s <= 4));
}
```

- [ ] **Step 2:** Run `cargo test governor::` and expect FAIL.
- [ ] **Step 3:** Implement the rules above. No Windows types and no I/O.
- [ ] **Step 4:** Run `cargo test governor::` and expect PASS.
- [ ] **Step 5:** Commit: `feat(governor): pure fps governor state machine`.

### Task 12: Knob shared state and object draw distance

**Files:**
- Create: `src/knobs/mod.rs`, `src/knobs/object_distance.rs`

**Interfaces:**
- Consumes: `sigs::SCENE_SETTINGS_FN`, `minhook`, `guard`, `frame_timer::qpc_now`.
- Produces from `knobs/mod.rs`:
  - `pub struct KnobShared { value_bits: AtomicU32, baseline_bits: AtomicU32, baseline_seq: AtomicU32, last_seen_qpc: AtomicU64 }`
  - `impl KnobShared { pub const fn new(v: f32) -> Self; pub fn value(&self) -> f32; pub fn set(&self, v: f32); pub fn publish(&self, baseline: f32, now_qpc: u64); pub fn baseline(&self) -> (f32, u32); pub fn last_seen(&self) -> u64 }`. `publish` bumps `baseline_seq` only when the baseline changes.
  - `pub static OBJECT: KnobShared = KnobShared::new(1.0)`, `pub static SCATTER: KnobShared = KnobShared::new(90.0)`, `pub static TERRAIN: KnobShared = KnobShared::new(1.0)`
  - `pub static SCATTER_LEVEL: AtomicU32`
  - `pub const OBJECT_STEPS`, `SCATTER_STEPS`, `TERRAIN_STEPS: [f32; 5]`, with the values in Global Constraints.
- Produces from `object_distance.rs`:
  - `pub unsafe fn apply_to_scene(scene: *mut u8, factor: f32)`. It multiplies the f32 at `+0x33c` and the f32 at `+0x340` by `factor.clamp(0.5, 1.0)`.
  - `pub unsafe fn publish_from_scene(scene: *const u8, now: u64)`. It publishes `+0x33c` as the OBJECT baseline, stores the u32 at `+0x290` into `SCATTER_LEVEL`, and records `last_seen`.
  - `pub fn install(m: &Module) -> Result<(), String>`. It creates the minhook hook on the function found by `SCENE_SETTINGS_FN`. The detour type is `extern "system" fn(scene: *mut u8, settings: *const u8, extra: *const u32)`. It calls the trampoline, then `publish_from_scene`, then `apply_to_scene(scene, OBJECT.value())`.
  - The hook is enabled through minhook's queued enable, applied once at the end of `entry::start` together with the terrain hook (Task 14).

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn scales_both_fields_and_clamps() {
    let mut s = vec![0u8; 0x400];
    put_f32(&mut s, 0x33c, 1.0); put_f32(&mut s, 0x340, 1.0);
    unsafe { apply_to_scene(s.as_mut_ptr(), 0.72) };
    assert_eq!((get_f32(&s, 0x33c), get_f32(&s, 0x340)), (0.72, 0.72));
    put_f32(&mut s, 0x33c, 1.0); unsafe { apply_to_scene(s.as_mut_ptr(), 0.1) };
    assert_eq!(get_f32(&s, 0x33c), 0.5);
}
#[test] fn publish_bumps_seq_only_on_change() {
    let k = KnobShared::new(1.0); k.publish(1.0, 1); let (_, a) = k.baseline();
    k.publish(1.0, 2); assert_eq!(k.baseline().1, a);
    k.publish(2.0, 3); assert_eq!(k.baseline().1, a + 1);
}
#[test] fn publishes_scatter_level() {
    let mut s = vec![0u8; 0x400]; put_u32(&mut s, 0x290, 3);
    unsafe { publish_from_scene(s.as_ptr(), 9) }; assert_eq!(SCATTER_LEVEL.load(Relaxed), 3);
}
```

- [ ] **Step 2:** Run `cargo test knobs::` and expect FAIL.
- [ ] **Step 3:** Implement. Call `install` from `entry::start` if `code_patches_allowed()` and ANY of the three distance knobs is enabled. The hook always publishes the baseline, `SCATTER_LEVEL` and `last_seen`, which is the governor's world-rendering signal. It calls `apply_to_scene` only when `knobs.enable_object_distance`. Add a test `hook_publishes_even_when_object_knob_off` that runs the detour body with the knob off: the scene stays unscaled and `last_seen` is updated.
- [ ] **Step 4:** Run `cargo test knobs::` and expect PASS.
- [ ] **Step 5:** Commit: `feat(knobs): shared knob state and object draw distance hook`.

### Task 13: Grass and scatter distance

**Files:**
- Create: `src/knobs/scatter_distance.rs`

**Interfaces:**
- Consumes: `sigs::{SCATTER_INSTANCE_LOADS, SCATTER_BRICK_LOAD, SCATTER_CAP_CONST, SCATTER_CAP_ALLOC}`, `patch::{alloc_near, rel32, write_checked, write_u32_checked}`, `knobs::SCATTER`.
- Produces:
  - `pub fn redirect_disp32(insn_end: usize, disp_addr: usize, new_target: usize) -> Option<[u8; 4]>`. It is pure and returns the little-endian disp32 bytes.
  - `pub unsafe fn install(m: &Module, cap_ok: bool) -> Result<(), String>`. It:
    - allocates the near page;
    - writes the f32 90.0 there and publishes its address as `SCATTER`'s storage;
    - redirects both High-level disp32 fields to it;
    - if `cap_ok`, writes `0x28000` over `0x14000` at both capacity immediates.
  - `pub const CAP_SITES: &[usize]`. These are the capacity sites accepted by the audit, as offsets from their signatures.
  - `pub fn max_step(cap_ok: bool, cfg_max: u8) -> u8`. It returns `0` when `!cap_ok`, because 90 m is the limit without more capacity.
  - `pub static SCATTER_FLOAT: AtomicUsize` holds the address of the page float, or 0 if it is not installed.
  - `pub fn set_distance(m: f32)` makes an aligned 4-byte store to that float (a no-op if 0) and mirrors the value into `SCATTER.set(m)` for logging. The game reads only the page float.
  - **Only when `SCATTER_LEVEL == 3`:** below High, the governor thread treats this knob as disabled.

- [ ] **Step 1: Capacity audit** (research step; its result decides `cap_ok`).
  - Scan the scatter functions in the real exe for every 4-byte immediate or displacement equal to `0x14000`, `0x28000` or a multiple of `0x14000` used as a size. The functions are: `FUN_140074b50` (shader-data filler), the function containing `0x1400745e0` (the allocation) and `FUN_1400767a0`, plus their direct callees. Use the scratch probes (`func.py`, `callers.py`) or REA.
  - Record every hit with its address and role in `docs/re-notes.md`, under "Scatter capacity audit".
  - **Result rule:** `cap_ok = true` only if every hit is one of the two known sites, or is added to `CAP_SITES` with its own signature and its own unit test of uniqueness. Otherwise `cap_ok = false`, and the knob ships with a maximum of 90 m, i.e. inactive.
- [ ] **Step 2: Write the failing tests:**

```rust
#[test] fn disp32_points_at_new_target() {
    let insn_end = 0x1400_7693e; let target = 0x1500_0000;
    let d = redirect_disp32(insn_end, insn_end - 4, target).unwrap();
    assert_eq!(i32::from_le_bytes(d) as i64, target as i64 - insn_end as i64);
}
#[test] fn out_of_range_target_refused() { assert!(redirect_disp32(0x1400_0000, 0x1400_0000 - 4, 0x3_0000_0000).is_none()); }
#[test] fn no_capacity_means_no_steps() { assert_eq!(max_step(false, 4), 0); assert_eq!(max_step(true, 4), 4); }
#[test] fn scatter_steps_respect_capacity_rule() {
    for m in SCATTER_STEPS { assert!(1.0 * m * m <= 2.0 * 8100.0 + 1.0); } // density 1.0, doubled capacity
}
```

- [ ] **Step 3:** Run `cargo test scatter_distance::` and expect FAIL.
- [ ] **Step 4:** Implement. The disp32 locations are signature match +30 (instance) and +9 (brick). The instruction end is the disp location + 4. Call it from `entry::start` if `code_patches_allowed()` and `knobs.enable_scatter_distance`. It must run before the scene is built, which is guaranteed because `InitializeASI` runs before WinMain.
- [ ] **Step 5:** Run `cargo test scatter_distance::` and expect PASS. Run the ignored test with `cargo test --lib -- --ignored --nocapture scatter_sigs_unique_in_game_exe` (see the run recipe in Task 4 Step 5) and expect PASS.
- [ ] **Step 6:** Commit: `feat(knobs): grass distance redirect with audited capacity`.

### Task 13b: Scatter counter probe (added 2026-10-25; the author: "Add it to v1")

Spec: `docs/design.md` sections 7.2 ("Capacity") and 7.2a. Evidence: `(the author's notes, not in this repo)` (the pointer chain is in its "brief changes" list, and the refuter's check confirmed the counter layout from the bytes).

**Files:**
- Create: `src/features/scatter_probe.rs`
- Modify: `src/sigs.rs` (add `SCATTER_FRAME_FN` to `ALL`), `src/config.rs` (add the `[Debug]` section with `ScatterProbe`)

**Interfaces:**
- Consumes: `scan::{Module, Pattern, find_in_sections}` (start-up only), `sigs::SCATTER_FRAME_FN`, `minhook`, `guard::{guard, Feature::ScatterDistance}`.
- Produces:
  - `pub const SCATTER_FRAME_FN: Sig = Sig { name: "SCATTER_FRAME_FN", pattern: "48 89 54 24 10 55 53 57 41 55 41 56 41 57", sections: &[".text"], patch_offsets: &[0] }`. It is unique in build f68140a3 at 0x1400767a0 (controller, 2026-10-26). The existing sigs tests extend to cover it; it is a prologue, so it has no imm sites.
  - `pub struct CounterSample { pub last: u32, pub peak: u32, pub warn: u32, pub cap: u32 }`
  - `pub const WARN_A: u32 = 0xf000; pub const CAP_A: u32 = 0x14000;`
  - `pub unsafe fn read_counters(pass: usize, readable: impl Fn(usize, usize) -> bool) -> Option<[CounterSample; 3]>`. It follows the chain below, calls `readable(addr, len)` before EVERY read, and returns `None` if any check fails or if counter A's `warn != WARN_A` or `cap != CAP_A` (the self-check). Chain: obj = [pass+0x18]; pool = [[pass+0x8]+0x20]; base = [pool+0x48]; entry(h) = base + (h & 0xffff) × 0x40, for h = u32 at [obj+0x8] (A), [obj+0xc] (B), [obj+0x10] (C). Each sample: last = u32 at entry+0x14, peak = u32 at +0x1c, warn = u32 at +0x20, cap = u32 at +0x24.
  - `pub fn is_readable(addr: usize, len: usize) -> bool`: VirtualQuery says MEM_COMMIT, a readable protection, not PAGE_GUARD, and the range fits in one region. Reuse Task 4's protection logic if it is public; otherwise copy its tested rule and say so.
  - `pub fn headroom_ok(last: u32, r_now: f32, r_next: f32) -> bool` = `last as f64 * (r_next / r_now)² < WARN_A as f64`. False for a non-finite or non-positive r_now.
  - `pub fn ceiling_step(steps: &[f32], r_max: f32) -> usize`: the highest index whose distance is <= r_max; 0 if none is.
  - `pub const R_MAX_M: f32 = 90.0;` (the measured ceiling; stays 90.0 until the author's measurement session sets it).
  - Published state: `pub static PROBE_OK: AtomicBool` (false until the first self-check passes; false for good after a failure), `pub static LAST_A: AtomicU32`, `pub static PEAK_A: AtomicU32`, `pub static LAST_B: AtomicU32`, `pub static LAST_C: AtomicU32`, `pub static FAILED: AtomicBool`.
  - `pub unsafe fn install(m: &Module) -> Result<(), String>`. It is called from `entry::start` (Task 6) when `EnableScatterDistance = 1` or `[Debug] ScatterProbe = 1`. It finds `SCATTER_FRAME_FN` and creates a minhook pre-hook, queued and enabled with the other hooks. The hook body runs inside `guard(Feature::ScatterDistance, call_original, ...)`. Every 30th call it runs `read_counters(rdx, is_readable)` and publishes the result: success stores the values and sets `PROBE_OK`; failure sets `FAILED` and clears `PROBE_OK`. It then calls the original with the same arguments. It never logs and never allocates.
- `config.rs`: add `DebugCfg { scatter_probe: bool, force_step_object: Option<u8>, force_step_scatter: Option<u8>, force_step_terrain: Option<u8> }` under `[Debug]`. ForceStep keys take `0..=4`; absent means None; out of range clamps with a warning. All of it is honoured only when `LogLevel = debug`; the gating is the consumer's job (Task 15). This also covers the ForceStep keys from the pre-flight ruling for Task 15, so Task 15 only consumes them. `[Debug]` is NOT written into the default ini.

- [ ] **Step 1: Write the failing tests:**
  - `headroom_ok(0xf000 / 2, 90.0, 100.0)` is true (0x7800 × 1.234 < 0xf000); `headroom_ok(0xe000, 90.0, 100.0)` is false; `headroom_ok(1, 0.0, 100.0)` is false.
  - `ceiling_step(&[90.0, 100.0, 110.0, 120.0, 127.0], 112.0) == 2`; `ceiling_step(.., 80.0) == 0`; `ceiling_step(.., 127.0) == 4`.
  - `read_counters` on a fake chain built in a test-owned buffer of real addresses gives A = {last, peak, 0xf000, 0x14000} and the B and C values; changing A.warn to 0xe000 gives None; a `readable` that refuses one link gives None, and that link is never read (the closure records the calls).
  - `is_readable` on a `VirtualAlloc(PAGE_NOACCESS)` page is false, and on a `PAGE_READWRITE` page is true.
  - The sigs tests cover `SCATTER_FRAME_FN` (it is in `ALL` and has offset 0). The ignored exe test finds it at 0x1400767a0.
  - Config: `[Debug]\nScatterProbe=1\nForceStepScatterDistance=7` gives scatter_probe true, force_step_scatter Some(4) and one warning; the default file has no `[Debug]` and gives DebugCfg::default().
- [ ] **Step 2:** Run `cargo test --lib -- scatter_probe:: sigs:: config::` and expect FAIL.
- [ ] **Step 3:** Implement. `read_counters` uses `core::ptr::read_unaligned` on each checked address.
- [ ] **Step 4:** Run the tests and the ignored exe test (`-- --ignored --nocapture`, see the run recipe in Task 4 Step 5); expect PASS.
- [ ] **Step 5:** Commit: `feat(scatter): GPU counter probe with self-check and headroom rule`.
- [ ] **Step 6 (the author, after Task 17's install):** the measurement session from spec 7.2a. The controller computes r_max = 90 × sqrt(0xf000 / peak_A), rounds it DOWN to a whole metre, sets `R_MAX_M`, records the measurement in RE-NOTES, and commits it as `chore(scatter): measured ceiling r_max = N m`.

Task 15 consumes this module. The governor's knob 1 is enabled only while `SCATTER_LEVEL == 3 && EnableScatterDistance && PROBE_OK`. A raise from step s to s+1 needs `s + 1 <= ceiling_step(SCATTER_STEPS, R_MAX_M)` and `headroom_ok(LAST_A, r_s, r_{s+1})`. If `LAST_A >= WARN_A`, step down one at once. If `FAILED` becomes true, reset to step 0 and log it once. With `[Debug] ScatterProbe = 1` and `LogLevel = debug`, the governor thread logs A, B and C every 2 s.

---

### Task 14: Terrain detail distance

**Files:**
- Create: `src/knobs/terrain_distance.rs`

**Interfaces:**
- Consumes: `sigs::TERRAIN_UPDATE_FN`, `minhook`, `knobs::TERRAIN`.
- Produces:
  - `pub fn scale_params(p: &mut [f32; 8], factor: f32)`. It is pure:
    - `p[0] *= factor`;
    - if `p[1] < p[0]` after that, `p[1] *= factor`;
    - `p[2..]` is unchanged;
    - `factor` is clamped to `1.0..=2.0`.
  - `pub fn install(m: &Module) -> Result<(), String>`. It creates the minhook pre-hook `extern "system" fn(mgr: *mut u8, cam: *const f32, params: *mut [f32; 8])`. It publishes `params[0]` as the TERRAIN baseline, calls `scale_params(&mut *params, TERRAIN.value())`, calls the trampoline, and then restores `params` to its pre-scale values, so the caller's own copy is unchanged.
  - **Body check.** The signature covers only the function's prologue, so `install` also checks the body: the bytes `8b 86 40 c4 00 00` (`mov eax,[rsi+0xc440]`) must appear within the first 0x80 bytes after the prologue. Otherwise it refuses.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn doubles_dist0_keeps_order() {
    let mut p = [32.0, 128.0, 256.0, 0.0, 0.0, 0.0, 0.0, 0.0]; scale_params(&mut p, 2.0);
    assert_eq!(&p[..3], &[64.0, 128.0, 256.0]);
}
#[test] fn raises_dist1_when_overtaken() {
    let mut p = [32.0, 40.0, 256.0, 0.0, 0.0, 0.0, 0.0, 0.0]; scale_params(&mut p, 1.5);
    assert_eq!(&p[..3], &[48.0, 60.0, 256.0]);
}
#[test] fn factor_clamped() { let mut p = [32.0; 8]; scale_params(&mut p, 9.0); assert_eq!(p[0], 64.0); }
```

- [ ] **Step 2:** Run `cargo test terrain_distance::` and expect FAIL.
- [ ] **Step 3:** Implement. Before relying on the body bytes, confirm them against the real exe (`func.py` at `0x1401e2e80`), and correct them in `sigs.rs` if they differ.
- [ ] **Step 4:** Run `cargo test terrain_distance::` and expect PASS.
- [ ] **Step 5:** Commit: `feat(knobs): terrain LOD distance pre-hook`.

### Task 15: Start-up scales, VRAM budget and the governor thread

**Files:**
- Create: `src/features/startup_scales.rs`, `src/vram_budget.rs`, `src/governor_thread.rs`

**Interfaces:**
- Consumes: Tasks 7–14.
- Produces from `startup_scales`:
  - `pub fn scaled(original: f32, scale: f32) -> f32`. It returns `original * scale`.
  - `pub fn apply(m: &Module, cfg: &KnobsCfg) -> Vec<String>`. It writes:
    - deco High (1.5) × `deco_distance_scale`;
    - distant High (2.5) × `distant_objects_scale`;
    - voxel High (32, 128, 160) × `terrain_ring_scale`.
    - Each goes through `write_u32_checked` with the original f32 bits as the expected value. A scale of 1.0 means no write.
- Produces from `vram_budget`: `pub unsafe fn free_mib(gipa, instance, phys) -> Option<u64>`. It uses `vkGetPhysicalDeviceMemoryProperties2` with `VkPhysicalDeviceMemoryBudgetPropertiesEXT` and returns `budget − usage` of the largest `DEVICE_LOCAL` heap. It returns `None` when the driver does not list `VK_EXT_memory_budget`.
- Produces from `governor_thread`: `pub fn spawn(cfg: GovernorCfg, knobs: KnobsCfg, scatter_cap_ok: bool)`. Every 50 ms it:
  1. reads new frames from `FRAMES` and converts them to intervals in ms;
  2. sets `world_active = qpc_now() - OBJECT.last_seen() < 250 ms`;
  3. reads `period_ms` from `display` (refreshed every 5 s) and `free_mib` (every 2 s);
  4. works out `baseline_changed` from each knob's `baseline_seq`;
  5. runs `Governor::tick`;
  6. maps each `Action::Set{knob, step}`: knob 0 → `OBJECT.set(OBJECT_STEPS[step])`; knob 1 → `scatter_distance::set_distance(SCATTER_STEPS[step])`; knob 2 → `TERRAIN.set(TERRAIN_STEPS[step])`. Knob 1's `KnobSpec.enabled` is true only while `SCATTER_LEVEL == 3`, `scatter_cap_ok` and `enable_scatter_distance`; when that turns false, the step is reset to 0 first;
  7. logs each step change (this thread may log).

- [ ] **Step 1: Write the failing tests:** `assert_eq!(scaled(1.5, 0.5), 0.75)`, and `apply` with all scales at 1.0 returns a `Vec` of `"unchanged"` notes. Plus a glue test, `actions_map_to_knob_values`: given `Action::Set{knob:2, step:4}`, the mapping function `knob_value(2, 4)` returns `2.0`.
- [ ] **Step 2:** Run `cargo test startup_scales:: governor_thread::` and expect FAIL.
- [ ] **Step 3:** Implement. `spawn` runs from `entry::start` when `target_fps > 0`. The thread body is wrapped in `guard(Feature::Governor, ...)`.
- [ ] **Step 4:** Run `cargo test` (the full suite) and expect PASS. Run the host and expect exit 0. The log shows the governor started and inactive, because the world is not active.
- [ ] **Step 5:** Commit: `feat: start-up scales, VRAM budget and governor thread`.

### Task 16: NVIDIA Reflex

**Files:** `src/reflex.rs`, `src/vk/mod.rs`, `src/knobs/object_distance.rs`, `tests/vk_chain.rs`, docs.
Issue #27 authorizes this wiring and the Reflex present-path exception to AGENTS rule 5.
The maintainer also approved `guard::disable_silent` in `src/guard.rs` and the parsed-mode
initialization step in `src/entry.rs` before Vulkan interception.

**Interfaces and maintained rulings:**
- Preserve W19's `TimelineFix` and pure `plan_device` API.
- `decide_mode(cfg: &ReflexMode, game_menu: Option<u32>, vendor_nvidia: bool, ext_listed: bool) -> Option<bool>` uses 0/1/2 for saved off/on/boost. `Auto` is the default. Unknown saved values use auto.
- At device creation, query prerequisites and add the extension whenever NVIDIA support exists and the configured mode is not `Off`. The saved menu is not required yet. Copy all changed chain nodes; unknown layouts fail closed.
- On a rejected modified device or swapchain create call, retry the exact original create-info pointer once and disable Reflex with `driver refused`.
- On each successful new swapchain, set sleep mode from the current decision. Once the menu first becomes known, update the mode once if that changes the decision.
- Read the verified menu fields in the existing scene-settings post-hook (`docs/re-notes.md`, "Reflex and vsync menu values seen from the scene-settings hook"). Check `R8 == RDX + 0x54` without overflow before reading Reflex at `RDX-8` and vsync at `RDX-11`; invalid ranges publish unknown through Relaxed atomics. Preserve the original-once hook structure.
- Present order: `PRESENT_START`, real present, `PRESENT_END`, sleep, 10 ms timeline wait. First timeout/device-lost/out-of-date disables Reflex. After success, increment ID, then `SIMULATION_START` and `RENDERSUBMIT_START`. Cache the real result before post-work. No allocation, blocking lock or ordinary logging on present.
- **No submit wrapper or submission-ID chaining in v1.**
- `latency_report() -> Option<String>` reads and formats driver averages. Do not schedule it here; Task 15 owns the five-second background schedule. Native handles must remain valid for the entire report query.
- Preserve texture, priority, timer and plain-renderer behavior. Anti-Lag wiring remains Task 16b.

**TDD and verification:**
- Fake-driver `rearm_on_new_swapchain` and `timeout_disables_reflex` tests.
- Real-wrapper `tests/vk_chain.rs` cases for listed extension injection, original input-chain pointer preservation, exact-original retry and non-NVIDIA bypass.
- Guarded menu read and native post-hook tests; retain all Task 12 hook tests.
- Actual present-wrapper tests for parsed modes, ordered markers, first-failure disable, unchanged input/timestamps and original-once fallback through both panic guards.
- Preserve teardown/reader lifetime tests.
- Compile and capture relevant failing mutations, then restore each implementation.
- Fresh target directory: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, strict `cargo doc`, `cargo test`, then `cargo asi`.
- Report: **not verified in game; the maintainer runs the boot test**.
### Task 17: In-game verification on the author's machine

**Install** (needs the author's go-ahead; deploying into the game folder is the author's). The game is now the Steam install, `C:\Program Files (x86)\Steam\steamapps\common\Enshrouded\` (the old `C:\Games\Enshrouded` is being deleted):
- copy the loader v10.0.0 `dinput8.dll` (zip sha256 `3d5f2f2a…`) to `C:\Program Files (x86)\Steam\steamapps\common\Enshrouded\`;
- copy `enshrouded-headroom.asi` to `C:\Program Files (x86)\Steam\steamapps\common\Enshrouded\plugins\`;
- create `C:\Program Files (x86)\Steam\steamapps\common\Enshrouded\global.ini` with `[GlobalSets] DebugLog=1`;
- do not use the old `_perf\Launch-Enshrouded.ps1` launcher at the same time (a copy of the `_perf` files is in `reference\old-build\_perf`; the Steam folder has no `_perf`);
- the old install had a `winmm.dll` (an unknown loader). The Steam folder has none (no `winmm.dll`, `dinput8.dll`, `plugins` or `global.ini` there, checked 2026-10-26). So there is one loader only, and nothing to remove.

- [ ] **Step 1: Load check.** `dinput8.log` shows the trigger API and `enshrouded-headroom.asi` loaded. The mod log shows `game build: f68140a3-steam; code patches on`. This is a Steam exe: its entry point is in Steam's DRM stub (`.bind`), which runs before the game's own entry. Note when `InitializeASI` runs relative to the stub, and that the game still starts and stays up with the code patches on (`AsyncCompute=on` patches `.text`). See RE-NOTES, "The Steam exe".
- [ ] **Step 2: Start-up features.** The game log shows `Setting texture streaming budget to 4,941,938,688 Bytes.` and `[render] async compute is enabled`.
- [ ] **Step 3: Vulkan.** The mod log shows `wrapper chain engaged`, and Reflex enabled or the reason it is off.
- [ ] **Step 4: Direction test.** Set `[Knobs]` so that each knob is forced to its maximum (add a debug key `ForceStep<Name>=4`, only honoured when `LogLevel=debug`). At a fixed spot, take screenshots at step 0 and step 4 for each knob.
  - Object, grass and terrain distance must visibly go up.
  - Any knob that fails is set `Enable<Name>=0` by default, and the result is recorded in `docs/re-notes.md`.
- [ ] **Step 5: Raise test.** With Models = Balance (to create headroom), run PresentMon for 60 s with the governor on. Steps go up, and fps stays at or above 75 except brief dips.
- [ ] **Step 6: Reflex test.** Compare the mod-logged latency with `Reflex=on` and with `Reflex=off`, at the same spot and the same fps.
- [ ] **Step 7: Vsync test.** Vsync on, no VRR. The governor log shows quantised mode and no flapping.
- [ ] **Step 8: Soak test.** 30 minutes of play, including an alt-tab and a resolution change: no crash and no hang. Record all results in `docs/re-notes.md` and commit: `test: in-game verification results`.

### Task 18: Package and release preparation

**Files:**
- Create: `scripts/package.ps1` (first line `# WHY NOT RUST: release zipping with Compress-Archive and a hash check; no build step needed`), `README.md` (full)

- [ ] **Step 1: Write `package.ps1`.** It:
  - runs `cargo asi`;
  - downloads the loader zip from the v10.0.0 release into `dist\cache\`;
  - verifies sha256 `3d5f2f2a47f456db878d17c1b135b0eae3209a7d8dabb0ff3f84e1b008c4f3a7` and refuses on a mismatch;
  - builds `dist\enshrouded-headroom-<version>.zip` with the spec section 4 layout, `ultimate-asi-loader-LICENSE.txt` (the MIT text) and the default `.ini`.
- [ ] **Step 2: Verify the artefact.** List the zip contents. Hash the `.asi` inside the zip and compare it with `target\release\enshrouded-headroom.asi`.
- [ ] **Step 3: Write `README.md`.** What it does, install, uninstall, the `.ini` reference (every key with its default and range), the tested build `f68140a3`, a note that a game update turns the knobs off until the mod is updated, and the loader credit.
  - Leave a section `## About AI assistance` with the line `TODO: the author writes this section in the author's own words.` This is the one deliberate placeholder in the plan, because the text must be the author's.
- [ ] **Step 4: Release gate.** the author reads all code and writes the disclosure. Only then create the public GitHub repo and push, which needs the author's explicit go-ahead in chat. The Nexus upload is the author's.
- [ ] **Step 5:** Commit: `chore: packaging and README`.
