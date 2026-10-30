# Codex worksheet

Tasks that fit work done from this repo alone. Read [`AGENTS.md`](../AGENTS.md) first; its rules apply to every task.

- Do one task per PR, from a new branch off `feat/v1`, into `feat/v1`.
- Do the tasks in the order given. W1 comes first, so later PRs get CI checks.
- Each task ends with **Done when**. A PR that does not meet it is sent back.

---

## W1. CI workflow

**Why:** PRs have no checks yet. Windows CI runs the tests that cannot run on Linux.

**Do:** add `.github/workflows/ci.yml` with exactly this content:

```yaml
name: CI

on:
  push:
    branches: [main, "feat/**"]
  pull_request:

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: true

jobs:
  check:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4

      # rust-toolchain.toml pins the toolchain (with clippy and rustfmt); rustup installs it here.
      - name: Toolchain
        run: rustup show active-toolchain

      - name: Format
        run: cargo fmt --check

      - name: Clippy
        run: cargo clippy --all-targets -- -D warnings

      # The tests that read the real game exe or need an interactive desktop are #[ignore]d.
      - name: Test
        run: cargo test

      - name: Build the .asi
        run: cargo asi

      - uses: actions/upload-artifact@v4
        with:
          name: enshrouded-headroom-asi
          path: target/release/enshrouded-headroom.asi
```

Also add `.claude/` to `.gitignore` (local agent worktrees).

**Done when:** the workflow runs on the PR and all steps pass.

---

## W2. Process priority (ported from the author's launcher), and the perf findings doc

**Why:** the author's earlier launcher made three changes. The plugin already makes two of them (texture budget, async compute). The third is AboveNormal process priority after the renderer is up. The game resets the priority before renderer creation, so the plugin must set it after that point.

**Do:**
1. **Config:** a new `[General]` key `Priority = above_normal | normal`, default `above_normal`.
   - Follow the parser style in `src/config.rs`; a bad value warns and uses the default.
   - Add it to `DEFAULT_INI` and `enshrouded-headroom.default.ini`.
   - `normal` means: do not change the priority.
2. **New file `src/features/priority.rs`:**
   - a pure `fn decide(setting) -> Option<u32>` (the priority class, or None);
   - `pub fn apply(setting)`. It calls `SetPriorityClass(GetCurrentProcess(), ABOVE_NORMAL_PRIORITY_CLASS)` and logs one info line: `priority: AboveNormal` or `priority: unchanged (setting)`. On failure it logs a warning with `GetLastError()` and carries on.
   - Register the module in `src/features/mod.rs`. Add any windows-sys feature that is needed.
3. **When:** call it once, in the post-work of the first game device's `vkCreateDevice` in `src/vk/mod.rs`, next to the texture-budget step.
   - Never call it on the present path.
   - It must not panic. Keep the existing `guard` structure.
4. **Tests:**
   - parse tests for the key: the default, both values, a bad value and upper case;
   - a test for `decide`.
5. **Docs:**
   - Create `docs/perf-findings.md` from the appendix at the end of this file. Add this line at the top: "Measured by the author on 2026-10-23. The plugin now makes all three launcher changes in-process; do not use the launcher and the plugin together."
   - In `docs/design.md` and `docs/re-notes.md`, change references to the author's 2026-10-23 measurement notes, or `_perf\FINDINGS.md`, to link `docs/perf-findings.md`.
   - Add `Priority` to the settings table in `docs/design.md` section 8.

**Done when:** CI passes; the new tests fail if `decide` or the parser branch is removed; the docs link the new file.

---

## W3. Docs debt from earlier reviews

**Why:** these doc lines no longer match the code.

**Do:**
1. `docs/plan.md:320`: `write_checked` no longer uses `PAGE_EXECUTE_READWRITE` for every page. Read `src/patch.rs`: non-executable pages get `PAGE_READWRITE`, and each region's own protection is restored. Make the plan line say what the code does.
2. `docs/plan.md:499` and `:845`: the statics `GAME_DEVICE` and `GAME_PHYS` were replaced by `pub fn vk::game_device() -> Option<GameDevice { device, phys }>` (see `src/vk/mod.rs`). Update both lines.
3. `docs/design.md` (async compute, "The patch is skipped if"): the code skips the patch when ANY command-line argument contains `async-compute` (case-insensitive, quotes removed); see `decide` in `src/features/async_compute.rs`. Make the spec say that.
4. `src/display.rs` module doc (lines 5-8) says the functions "keep no state" and "ask Windows afresh". After a callback panic the guard's process-wide flag makes `game_window` return None. Add one sentence that says so. This is a comment change only.
5. `docs/design.md` test-environment note (line 31): it says there is "one loader", but nothing is installed yet. Say that this is the state after the install step (plan Task 17).

**Done when:** each item is changed, nothing else in the docs is changed, and CI passes.

---

## W4. One PE parser instead of four

**Why:** four functions walk PE headers by hand:
- `scan.rs` (`parse_pe` or similar);
- `text_of` in `features/async_compute.rs`;
- `rdata_of` in `features/texture_budget.rs`;
- the test-only parser in `build_id.rs`.

They can drift apart.

**Do:**
- Make one `pub(crate)` PE section parser, in `src/scan.rs` or a new `src/pe.rs`. It is bounds-checked and returns `Result`; it never panics.
- Make the other three use it. Keep every public behaviour and every existing test.
- Move the synthetic-PE tests from `build_id.rs` (`tiny_pe`, `two_section_pe` and the tests that use them) so they test the shared parser.

**Done when:** a grep finds one PE header walker; all tests pass; no `#[ignore]` test changed its assertions.

---

## W5. Retire `config::load_or_create`

**Why:** since the settings fallback fix, `load_or_create` has no production caller. Start-up uses `read_ini` and `write_default` through `entry::load_settings`.

**Do:**
- Delete `load_or_create`.
- Move each of its tests onto `read_ini` / `write_default` (or onto `entry::load_settings`), keeping what each test proves.
- Update `docs/plan.md` where it names `load_or_create` as an interface; say it was replaced.

**Done when:** a grep finds no `load_or_create` in `src/`; the test count does not drop; CI passes.

---

## W6. A full README

**Why:** the README is three lines. Players and reviewers need more.

**Do:** write `README.md` with these sections:
- what the mod does (the four features in AGENTS.md, marked "planned" or "in progress" where true);
- status (work in progress, not tested in-game yet);
- install (the package layout in `docs/design.md` section 4, with Ultimate ASI Loader v10.0.0 as `dinput8.dll`);
- settings (generated from `enshrouded-headroom.default.ini`: every key, its default and its meaning);
- logs and troubleshooting (the `.log` next to the plugin, or `%LOCALAPPDATA%\enshrouded-headroom\`);
- uninstall;
- build from source (the commands in AGENTS.md);
- licence (MIT, plus the loader's MIT licence);
- credits (Ultimate ASI Loader by ThirteenAG).

Add a final section `## About AI assistance` that contains only the line `TODO: the author writes this section.` Do not write that text yourself.

**Done when:** the README has those sections, every settings key in the default ini is listed, and nothing in it is a claim the code does not support.

---

## W7. Release package script (plan Task 18, step 1 only)

**Why:** a release zip must contain the loader, the plugin, the default ini and the licences, in the layout of `docs/design.md` section 4. The loader must be downloaded and hash-checked, never committed.

**Do:**
- Write `scripts/package.ps1`. Its first line is `# WHY NOT RUST: release zipping with Compress-Archive and a hash check; no build step needed`.
- It must:
  1. run `cargo asi`;
  2. download `Ultimate-ASI-Loader_x64.zip` from the GitHub release v10.0.0 of `ThirteenAG/Ultimate-ASI-Loader` into `dist\cache\` (only if it is not already there);
  3. verify its SHA-256 is `3d5f2f2a47f456db878d17c1b135b0eae3209a7d8dabb0ff3f84e1b008c4f3a7`, and refuse on any mismatch;
  4. build `dist\enshrouded-headroom-<version>.zip` with:
     - `dinput8.dll`;
     - `ultimate-asi-loader-LICENSE.txt` (the loader's MIT text);
     - `plugins\enshrouded-headroom.asi`;
     - `plugins\enshrouded-headroom.ini` (the default ini);
     - `LICENSE`;
  5. print the zip contents and the SHA-256 of the `.asi` inside it.
- `<version>` comes from `Cargo.toml`. Add `dist/` to `.gitignore`.

**Done when:** the script is in the PR, and the PR text says it was not run on Windows if you could not run it. Do not publish a release.

---

# Part 2 (W8-W12)

Start these only after the PR for the task they depend on is merged. The dependency is noted on each task. As before, do one task per PR, from a fresh branch off the current `feat/v1`, into `feat/v1`.

## W8. Property tests for the settings parser (after W5, PR #6, is merged)

**Why:** the `.ini` is hand-edited by players. The parser must never panic and must always fall back sanely, whatever the input.

**Do:** add `proptest` as a **dev-dependency** (the current release; give the reason in the PR). Then add property tests in `src/config.rs`'s test module:
- `config::parse` on any random string (including invalid UTF-8 turned into U+FFFD, very long lines, NUL bytes and `\r`) never panics. Every returned value is inside its documented range.
- `parse(DEFAULT_INI)` gives `Config::default()` with no warnings.
- For every numeric key: a random value outside its range gives the default or the clamped value, plus exactly one warning that names the key.
- A decimal comma (`0,9`) is read like `0.9` wherever the docs say so.
- Keep each property's case count low enough that `cargo test` stays under about 10 s more on CI.

**Done when:** CI passes; removing any range check in the parser makes at least one property fail. Say in the PR which check you removed to show it.

## W9. Property tests for the byte-pattern scanner (after W4, PR #5, is merged)

**Why:** `src/scan.rs` runs inside the game at start-up. A wrong match patches the wrong bytes.

**Do:** with the same `proptest` dev-dependency, add property tests:
- Pattern parsing on random strings never panics. A valid pattern string round-trips (bytes plus wildcards).
- `find_unique` (or the current equivalent) on random byte buffers with random patterns, some of them planted, always agrees with a naive reference search written inside the test:
  - zero matches → none;
  - exactly one → that offset;
  - two or more → the "not unique" error.
- It never panics, including on an empty buffer, an empty pattern and an all-wildcard pattern.

**Done when:** CI passes; a deliberately broken `find_unique` (an off-by-one at the buffer end) fails a property. Say how you checked that.

## W10. CI improvements (any time)

**Do:** in `.github/workflows/ci.yml`:
- add `Swatinem/rust-cache@v2` after checkout;
- add a `cargo doc --no-deps` step with `RUSTDOCFLAGS: -D warnings` (fix any broken intra-doc links it finds; doc text only);
- write the test summary lines (`test result:`) to `$GITHUB_STEP_SUMMARY`.

Keep the job under about 5 minutes.

**Done when:** CI passes on the PR and the summary shows the test counts.

## W11. Troubleshooting guide (after W6, PR #7, is merged)

**Why:** players will send the plugin log. Each line in it should be explainable.

**Do:** create `docs/troubleshooting.md`:
- For every message the plugin can log (grep the `log::info!`, `log::warn!` and `log::error!` format strings in `src/`), say what it means and what the player should do.
- Group the messages by feature: start-up and build check, settings, texture budget, async compute, Vulkan hooks, display.
- Quote each message's fixed text exactly. Do not invent messages that are not in the code.
- Link the guide from the README's troubleshooting section.

**Done when:** a check you describe in the PR shows that every `info!`, `warn!` and `error!` format string in `src/` (outside tests) appears in the guide.

## W12. Issue template and changelog (after W6, PR #7, is merged)

**Do:**
- Add `.github/ISSUE_TEMPLATE/bug_report.yml`. It is a form that asks for:
  - the plugin log (`enshrouded-headroom.log`, attached);
  - the "game build:" line from that log;
  - the GPU and driver;
  - the `.ini` changes;
  - steps to reproduce;
  - what happened and what was expected.
- Add `.github/ISSUE_TEMPLATE/config.yml` with `blank_issues_enabled: false`.
- Add `CHANGELOG.md` (Keep a Changelog format) with an `Unreleased` section that summarises what is on `feat/v1`, from `git log` and the merged PRs. Use plain words and no personal names.

**Done when:** the issue form renders on GitHub (YAML is valid) and the changelog matches the merged work.

---

## Appendix: text for `docs/perf-findings.md` (W2)

> # Enshrouded performance findings (2026-10-23)
> 
> Target: `enshrouded.exe`, sha256 `b28b448a…c12a`, build `f68140a3`, log app version 1273043.
> Rig: RTX 5080 16 GB, Ryzen 9 5900X (2 CCDs, 24 threads), 4K 120 Hz HDR.
> Tools: REA 6.3.0 with Ghidra 12.1.4, and a capstone byte scan. Evidence IDs are REA records.
> 
> ## Applied by `Launch-Enshrouded.ps1`
> 
> | Change | Evidence | Proof that it landed |
> |---|---|---|
> | Texture budget for cards with 10 GiB or more: 2 GiB → 6 GiB, in memory only | `FUN_1400f82c0` picks the budget from table `DAT_1413b86d0`. The rows are VRAM thresholds 0 / 4.5 / 6 / 8 / 10 GiB, with budgets 0.29 / 0.59 / 1.0 / 1.49 / 2.0 GiB. No row is above 10 GiB. (ev_eb387ca2…) | log: `Setting texture streaming budget to 6,442,450,944 Bytes.` |
> | `--enable-async-compute` | The parser at `0x14084f711` sets an optional bool. The renderer create (call at `0x14084912f`) uses it in place of the automatic choice. The automatic choice was **disabled** on this card. (ev_8b734aed…) | log: `[render] async compute is enabled` |
> | Priority AboveNormal | — | launcher output |
> 
> ## Checked and not needed
> 
> - **Vulkan overlay layers.** The game already sets the `DISABLE_*` variables for the Overwolf, ReShade, OBS, RTSS, MangoHud, vkBasalt, EOS and Bandicam layers. Only `--keep-vulkan-layers` stops this (strings at file offsets `0x1da8360`–`0x1da84f8`).
> 
> ## VRAM facts (from the game's own log)
> 
> - The game's peak device-local allocation in a full session at max settings was **8.6 GiB** of 15.6 GiB. That peak included the 2 GiB texture pool.
> - Other pools in the same function are also capped by VRAM tier, at the 8 GiB tier or below (`DAT_1413b87a0`, `DAT_1413b87d0`). One of them is the 128 MiB streaming upload buffer. These were not changed.
> 
> ## Not yet tried (candidates)
> 
> - `--large-pages`. The game calls its large-page setup. It needs the "Lock pages in memory" user right, which only the author can grant (secpol). Without the right, the game logs `Couldn't enable large pages` and continues.
> - `--enable-dxgi`. The behaviour is not analysed.
> - Vsync. The game always uses `FIFO` present mode. At 120 Hz without VRR, a frame slower than 8.3 ms waits for the next refresh, so the frame rate drops to 60. Check whether G-SYNC is on.
> 
> ## Result
> 
> - 2026-10-23: the author reports that fps is better with the launcher. The change that caused it is not isolated. To isolate it, run with `-NoAsyncCompute` and compare.
> - 2026-10-23, live measurement over 20 s in-game, with the launcher, 4K, DLSS Performance, max settings:
>   - PresentMon, 1363 frames: average 68.3 fps, 1% low 54.2 fps. Average frame 14.63 ms, of which the GPU was busy 14.55 ms (GPUWait 0.08 ms). Present mode `Hardware: Independent Flip` on every frame.
>   - nvidia-smi, 20 samples: GPU utilisation 96 % average (91–97 %), 2856 MHz, 282 W.
>   - **The game is GPU-bound now.** This replaces the earlier guess from the DLSS steps that the limit was not the GPU.
>   - VRAM: the adapter used 15,334 of 16,303 MiB. The game's own log total is 11.88 GiB, compared with 8.6 GiB before the budget change. DWM's "dedicated" counter read 3,487 MiB in one sample and 1,743 MiB averaged over 10 s; this counter is noisy. Adapter total in that second read: 15,349 MiB. Headroom is about 1 GiB.
>   - CPU, 10 s over 5 samples (Processor Utility): 28 % total. The busiest logical CPUs were at 63–73 %, so no core was saturated. This agrees with the GPU-bound reading. It was not confirmed that the author was in gameplay during this sample.
>   - If hitches show up while travelling, use `-TextureBudgetGiB 4`.
>   - The NVIDIA Overlay process reports 25,386 MiB "dedicated" but only 174 MiB committed. This matches "overlay seems busted". The figure is not real VRAM use.
