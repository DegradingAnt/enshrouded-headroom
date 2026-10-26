# 0.1.0 release checklist and Nexus draft

This is the author's future release procedure. W28 prepares documents and validates local packaging; it does not authorize a tag, a merge to main, a GitHub release or a Nexus upload. A zip built from the present branch is a preparation artifact, not a release candidate with completed runtime integrations.

## Integration prerequisites

- [ ] Merge and review W23 (#27, Reflex), W24 (#30, governor) and W25 (#31, Anti-Lag) before claiming their runtime features.
- [ ] Resolve the parsed-but-unapplied startup scale settings and record whether they are in 0.1.0.
- [ ] Preserve the grass ceiling until capacity measurements justify changing it. W27's background sampler is merged; the author's probe-on game validation and capacity evidence are still required.
- [ ] Update README, descriptions and changelog from the final source; regenerate the settings table with `cargo run --example generate_readme_settings` and verify it with `-- --check`.
- [ ] Keep Cargo.toml at 0.1.0 and verify the changelog top entry against the actual startup child log.
- [ ] Run fresh-target fmt, strict clippy, strict docs and tests, then the real `scripts/package.ps1` once; record its six-entry listing, verified loader hash, source/inner ASI equality and zip SHA-256.
- [ ] Run the isolated staged-ASI corruption fixture and confirm it refuses before replacing any final package.

## 1. Complete in-game verification

These are [plan Task 17](plan.md#task-17-in-game-verification-on-the-authors-machine), Steps 1–8. The author authorizes and performs deployment to the Steam folder and retains the results. Locate it through Steam Properties → Installed Files → Browse. Use one compatible x64 ASI loader, preserve other plugins and back up the INI. For this diagnostic run, enable the loader's `[GlobalSets] DebugLog=1`. Do not run a second launcher that changes the same texture/async settings.

1. **Load check.** On `f68140a3-steam`, verify the loader log names the trigger and loaded ASI and the mod log says `game build: f68140a3-steam; code patches on`. Record startup relative to the Steam `.bind` stub and confirm the game stays running with code patches enabled.
2. **Startup features.** Check the game's texture-streaming budget and async-compute lines. The plan's recorded machine-specific budget expectation is `4,941,938,688 Bytes`; verify it against the actual GPU/INI calculation rather than treating it as universal. Check `[render] async compute is enabled` for the intended on/default run.
3. **Vulkan integration.** Verify `wrapper chain engaged` and the final integrated Reflex enabled/off reason; confirm the governor and Anti-Lag startup outcomes match the selected configuration.
4. **Distance direction.** After the integrated debug forcing path is available, use `LogLevel=debug` and each `ForceStep<Name>=4` separately; compare screenshots against step 0 at a fixed spot. Objects, grass and terrain must move in the intended direction within their supported ceilings. Do not bypass the grass capacity limit. Disable any failing knob by default and record it in re-notes.
5. **Governor raise test.** With Models = Balance and the governor integrated, collect a 60-second PresentMon run. Check that steps rise when there is room and frame rate stays at or above the 75 fps target except brief dips; record actual observations, not an assumed pass.
6. **Reflex comparison.** Compare integrated mod latency reports with Reflex on/off at the same spot and frame rate. Retain settings and logs; do not claim improvement unless the comparison supports it.
7. **Vsync behavior.** With vsync on and VRR off, confirm quantised controller mode and no repeated oscillation between distance steps.
8. **Soak test.** Play for 30 minutes including alt-tab and a resolution change; check for crashes and hangs. Record every result and limitation in `docs/re-notes.md` and commit the verified observations.

Also compare HDR paper white/peak off versus on in HDR and confirm SDR remains unchanged. Keep `PaperWhiteNits=game` and `FixPeak=off` until those comparisons justify a change. Test AMD Anti-Lag on appropriate hardware or retain the explicit **not verified on AMD hardware** limitation. No Windows test suite substitutes for these checks.

If loading crashes, rendering is corrupted or a hang occurs, stop the release. With the game closed, remove this mod's ASI or restore the prior known package/INI; remove the shared loader only when no other plugin needs it. Preserve the failing logs and revisit the patch/integration before retrying.

## 2. Read the code

- [ ] The author reads the complete final code and diff, including unsafe/native lifetime paths, configuration, packaging and tests.
- [ ] Resolve every blocking review finding and confirm the tested commit matches the intended release commit.
- [ ] Confirm third-party licences/notices, loader version/hash and public-repository hygiene.

## 3. Write the disclosure

- [ ] The author writes `README.md`'s `## About AI assistance` section in the author's own words. W28 preserves the existing section byte-for-byte and does not complete it on the author's behalf.
- [ ] Check README, changelog and Nexus text against measured results and remaining limitations; remove preparation wording only when each gate is complete.

## 4. Merge the release branch

- [ ] The author approves and merges `feat/v1` into `main` through the normal reviewed process.
- [ ] Confirm CI is green on that exact merged commit and repeat package validation if the source changed.

## 5. Tag and publish, only with the author's authorization

- [ ] Set the changelog release date only when release is approved; retain the 0.1.0 package version.
- [ ] Create the `v0.1.0` tag on the approved commit and verify what it points to.
- [ ] Create the GitHub release with the validated zip and its SHA-256; include exact build, known limits, default settings and verification evidence.
- [ ] The author checks and uploads the final Nexus page and archive. Verify the downloaded archive hash against the release receipt.

## Nexus page text: unpublished draft

**enshrouded-headroom — Vulkan performance controls for Enshrouded**

Preparation status: the governor, NVIDIA Reflex and AMD Anti-Lag runtime integrations and in-game verification are pending. Do not publish this draft as a finished release. Update it after the gates above are satisfied.

This Windows x64 ASI plugin provides checked texture-budget sizing, async compute, distance-hook infrastructure and optional HDR adjustments for Enshrouded's Vulkan renderer. It is designed around preserving the player's menu baseline and leaving mismatching game code alone. No measured performance gain is claimed here.

Defaults: automatic texture budget with an 11 GiB reserve; async compute on; Above Normal priority; object/grass/terrain controls enabled with maximum configured step 4; planned governor target 75 fps; Reflex and Anti-Lag auto (runtime pending); HDR paper white game; HDR peak correction off. Grass remains capped at the game's 90 m until capacity is proven. Startup scales default to 1.0 and are currently parsed only. There are no shadows-above-Ultra controls in this base.

The Steam verification target is `f68140a3-steam`. An unrecognised game update disables code patches and hooks by default. `AllowUnknownBuild=1` permits attempts at the player's own risk; it does not establish compatibility. AMD Anti-Lag has not been verified on AMD hardware. The scatter probe is debug-only and off by default.

Install: close the game, browse its installed files through Steam, and extract the approved package beside the executable while preserving `plugins`. Do not replace another mod's shared loader without checking compatibility. Save the .ini as UTF-8, restart after edits, and check the mod log.

Update: close the game, back up settings/logs, replace the ASI and notices, and preserve your INI while comparing added defaults. Uninstall: close the game and remove only this mod's ASI, INI and log; remove the loader only if no other ASI plugin uses it. Memory changes end when the game exits.

See the README for all settings and the troubleshooting guide for log meanings. Include the game build, mod log and settings in a bug report. Ultimate ASI Loader v10.0.0 is by ThirteenAG; the package includes the mod's MIT licence and third-party notices. The author's AI-assistance disclosure belongs in the final README and release description.
