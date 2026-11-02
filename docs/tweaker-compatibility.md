# Using Headroom with Extended Graphics and Physics Tweaker

Headroom and Tweaker own different settings. Tweaker edits game resources and shaders for debris, particles, extra shadow-casting lights and water. Its live controls own fog-resource values and the Ultra global-illumination quality row. Headroom owns its texture streaming budget, checked startup scales, draw-distance factors, optional HDR settings and Vulkan latency/timing paths.

A bounded source comparison found no direct competing target. This does not prove that every combination works in the game. Headroom does not read Tweaker's state, select a GI preset, restore resources or change Tweaker settings. Neither mod can guarantee an FPS target when the chosen effects exceed the machine's capacity.

## Startup notice

During enabled startup, Headroom checks for two files beside the executable:

- `_debris_mod/state.json`
- `_debris_mod/tools/live.py`

Only when both file markers exist, it logs:

```text
Tweaker files detected: possible installation, activity not verified; GI and fog remain owned by Tweaker; combined runtime compatibility is unverified
```

This identifies a possible installation. It does not prove that Tweaker patches are active, that its manifest matches current game files, or that a live GI preset is selected. A directory named like a marker is insufficient. A missing marker, unavailable file metadata or unknown executable location emits no notice. Another installation folder is not detected.

The detector runs once at startup, after the `Enabled=0` early return. It does no JSON parsing, script execution, resource access, writes or present-path work. No new setting is needed.

## Workload and safety limits

Longer-lived effects, extra lights casting shadows, volumetric smoke and increased VFX distance can add GPU, VRAM and CPU cost. Headroom's governor reacts to its existing frame-time and VRAM observations; it does not reduce Tweaker's effects. Unknown VRAM still blocks costly raises, and unexpected object-distance baselines still skip scaling. This notice does not override either safety policy or change any quality setting.

## Combined boot checks still pending

These checks have not been performed and are not release evidence:

| Scenario | Check |
|---|---|
| Both installed, Tweaker defaults | Both tools start, normal rendering and native input remain intact. |
| Tweaker enabled with heavier effects | Frame times and VRAM remain acceptable; Headroom holds or undoes its own raises. |
| Live fog or GI changes | Tweaker changes remain visible; no claim is made that Headroom tracks its selected preset. |
| Headroom disabled | No compatibility notice or feature startup occurs after the disabled line. |
| Tweaker restored or removed | Marker presence is distinguished from active patches; missing markers produce no notice. |
| Game update | Each tool's own build/layout checks still refuse unproven changes. |

Back up the relevant logs before a controlled test. Do not replace loaders or apply resource patches while the game is running. No Tweaker parser or implementation is bundled or copied here; its redistribution license has not been established.