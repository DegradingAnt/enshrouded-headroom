//! Generate only the README settings table from the crate's actual DEFAULT_INI.
//! Run `cargo run --example generate_readme_settings`, or add `-- --check` to verify freshness.
use enshrouded_headroom::config::DEFAULT_INI;
use std::error::Error;
use std::path::Path;

/// Reviewed parser ranges and plain descriptions; keys/defaults/order come from DEFAULT_INI.
fn description(section: &str, key: &str) -> Option<(&'static str, &'static str)> {
    Some(match (section, key) {
        ("General", "Enabled") => ("0, 1 (also false, true)", "Enable feature startup while keeping settings and logging available when disabled."),
        ("General", "TextureBudget") => ("auto, off, 2–8 GiB", "Choose automatic or fixed texture budget sizing; integrated GPUs keep the original budget."),
        ("General", "TextureReserveGiB") => ("4–16 GiB", "Subtract this amount from the largest device-local heap in auto mode."),
        ("General", "AsyncCompute") => ("on, game", "Attempt the checked async-compute patch unless a launch argument mentions async-compute."),
        ("General", "Priority") => ("above_normal, normal", "Request Above Normal priority once after device creation, or leave priority unchanged."),
        ("General", "Reflex") => ("game, auto, off, on, boost", "Select a supported NVIDIA Reflex mode; game follows saved off/on/boost values."),
        ("General", "AntiLag") => ("auto, off, on", "Select experimental AMD Anti-Lag; enabled modes require AMD and driver support, and Reflex takes priority."),
        ("General", "AllowUnknownBuild") => ("0, 1 (also false, true)", "Permit checked code-patch attempts on unknown builds only at your own risk."),
        ("General", "LogLevel") => ("error, warn, info, debug", "Set the logging threshold."),
        ("Governor", "TargetFps") => ("0 (off), 30–360 fps", "Set the governor target, limited by display refresh; zero stops the governor thread."),
        ("Governor", "WindowSeconds") => ("0.5–10 seconds", "Choose the rolling frame-time measurement window."),
        ("Governor", "RaiseBelow") => ("0.5–0.99", "Require p95 headroom below this target-time fraction outside quantised vsync mode."),
        ("Governor", "RaiseHoldSeconds") => ("0.1–10 seconds", "Require the raise condition to hold for this long."),
        ("Governor", "SettleSeconds") => ("0.1–10 seconds", "Wait this long after a distance change before normal raise/lower decisions."),
        ("Governor", "LowerAbove") => ("0.8–1.0; greater than RaiseBelow", "Set the p95 overload fraction for undoing the latest distance raise."),
        ("Governor", "LowerHoldSeconds") => ("0.1–10 seconds", "Require the normal overload condition to hold before undoing a raise."),
        ("Governor", "HitchFactor") => ("1.2–5", "Set the post-raise hitch multiplier, with a display-period margin in quantised mode."),
        ("Governor", "BlockSeconds") => ("1–300 seconds", "Set the base undo block and probation duration; repeated undos can extend the block."),
        ("Governor", "MinVramFreeMiB") => ("0–8192 MiB", "Block costly raises below this free VRAM; unknown VRAM also blocks them."),
        ("Knobs", "EnableObjectDistance") => ("0, 1 (also false, true)", "Allow governor-controlled object-distance steps above the menu baseline."),
        ("Knobs", "MaxObjectDistance") => ("0–4", "Cap the object-distance step, with zero preserving the menu baseline."),
        ("Knobs", "EnableScatterDistance") => ("0, 1 (also false, true)", "Enable the checked grass-distance redirect, currently held at the game’s 90 m ceiling."),
        ("Knobs", "MaxScatterDistance") => ("0–4", "Cap the scatter step, further restricted by proven capacity headroom."),
        ("Knobs", "EnableTerrainDistance") => ("0, 1 (also false, true)", "Enable the terrain-distance hook and its governor-controlled steps."),
        ("Knobs", "MaxTerrainDistance") => ("0–4", "Cap the terrain-distance step, with zero preserving the menu baseline."),
        ("Knobs", "DecoDistanceScale") => ("0.5–1.0", "Scale the verified High decoration threshold at startup; 1.0 keeps the original."),
        ("Knobs", "DistantObjectsScale") => ("0.5–1.0", "Scale the verified High distant-object threshold at startup; 1.0 keeps the original."),
        ("Knobs", "TerrainRingScale") => ("1.0–1.6", "Scale the verified High terrain rings at startup; 1.0 keeps the originals."),
        ("Hdr", "PaperWhiteNits") => ("game, 40–400 nits", "Keep the game’s 250-nit paper white or apply a checked startup override."),
        ("Hdr", "FixPeak") => ("on, off", "Opt into HDR peak correction through the shared scene hook when a distance knob enables it."),
        ("Hdr", "DisplayPeakNits") => ("game, whole number 80–10000 nits", "Choose the game peak or panel target used only when FixPeak is on."),
        ("Shadows", "PointLightRays") => ("game, 65–256", "Experimental Ultra ray limit; game preserves vanilla."),
        ("Shadows", "PointLightStaticDistance") => ("game, 101–400 metres", "Experimental Ultra static-light distance; game preserves vanilla."),
        ("Shadows", "PointLightTile") => ("game, 224, 256", "Experimental Ultra atlas tile; increases VRAM use."),
        ("Shadows", "SunResolution") => ("game, 4096", "Experimental Ultra sun resolution; increases VRAM use."),
        ("Debug", "FrameStats") => ("0, 1 (also false, true)", "Permit frame reports with the governor off; requires LogLevel=debug."),
        _ => return None,
    })
}
/// Render every actual default, refusing an undocumented newly added key.
fn table() -> Result<String, Box<dyn Error>> {
    let mut section = "";
    let mut rendered = String::from(
        "\n| Section | Key | Default | Range or choices | Meaning |\n|---|---|---|---|---|\n",
    );
    for line in DEFAULT_INI.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = name;
        } else if !line.starts_with([';', '#']) {
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let (choices, meaning) = description(section, key)
                    .ok_or_else(|| format!("Missing settings description: [{section}] {key}"))?;
                rendered.push_str(&format!(
                    "| `{section}` | `{key}` | `{}` | {choices} | {meaning} |\n",
                    value.trim()
                ));
            }
        }
    }
    Ok(rendered)
}
/// Git may convert LF to CRLF on Windows; all non-newline content must still match.
fn table_is_current(current: &str, generated: &str) -> bool {
    current.replace("\r\n", "\n") == generated
}
/// Replace the marked table without touching the author's disclosure or other README bytes.
fn main() -> Result<(), Box<dyn Error>> {
    const START: &str = "<!-- BEGIN GENERATED SETTINGS -->";
    const END: &str = "<!-- END GENERATED SETTINGS -->";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    let mut readme = std::fs::read_to_string(&path)?;
    let start = readme
        .find(START)
        .and_then(|offset| offset.checked_add(START.len()))
        .ok_or("Missing settings start marker")?;
    let relative_end = readme
        .get(start..)
        .and_then(|rest| rest.find(END))
        .ok_or("Missing settings end marker")?;
    let end = start
        .checked_add(relative_end)
        .ok_or("Settings range overflow")?;
    let generated = table()?;
    if std::env::args().skip(1).any(|arg| arg == "--check") {
        if !readme
            .get(start..end)
            .is_some_and(|current| table_is_current(current, &generated))
        {
            return Err("README settings are stale; run the generator".into());
        }
    } else {
        readme.replace_range(start..end, &generated);
        std::fs::write(path, readme)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh Windows checkout may use CRLF even though the generated table uses LF.
    #[test]
    fn freshness_accepts_windows_checkout_newlines_but_rejects_changed_content() {
        let generated = table().unwrap();
        assert!(table_is_current(&generated, &generated));
        assert!(table_is_current(
            &generated.replace("\n", "\r\n"),
            &generated
        ));
        assert!(!table_is_current(
            &generated.replace("Enabled", "Missing"),
            &generated
        ));
    }
}
