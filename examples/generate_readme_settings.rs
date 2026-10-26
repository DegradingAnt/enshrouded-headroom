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
        ("General", "Reflex") => ("game, auto, off, on, boost", "Select the NVIDIA Reflex mode; runtime integration is pending."),
        ("General", "AntiLag") => ("auto, off, on", "Select the experimental Anti-Lag mode; runtime integration and AMD verification are pending."),
        ("General", "AllowUnknownBuild") => ("0, 1 (also false, true)", "Permit checked code-patch attempts on unknown builds only at your own risk."),
        ("General", "LogLevel") => ("error, warn, info, debug", "Set the logging threshold."),
        ("Governor", "TargetFps") => ("0 (off), 30–360 fps", "Set the planned governor target, capped by display refresh when runtime integration lands."),
        ("Governor", "WindowSeconds") => ("0.5–10 seconds", "Choose the planned frame-time measurement window."),
        ("Governor", "RaiseBelow") => ("0.5–0.99", "Allow a planned raise when the 95th-percentile frame time is below this target-time fraction."),
        ("Governor", "RaiseHoldSeconds") => ("0.1–10 seconds", "Require the planned raise condition to hold for this long."),
        ("Governor", "SettleSeconds") => ("0.1–10 seconds", "Wait this long after a planned distance change before deciding again."),
        ("Governor", "LowerAbove") => ("0.8–1.0; greater than RaiseBelow", "Undo a planned raise when the 95th-percentile frame time exceeds this target-time fraction."),
        ("Governor", "LowerHoldSeconds") => ("0.1–10 seconds", "Require the planned lower condition to hold for this long."),
        ("Governor", "HitchFactor") => ("1.2–5", "Undo a recent planned raise if a frame exceeds this multiple of the target frame time."),
        ("Governor", "BlockSeconds") => ("1–300 seconds", "Delay retrying a planned raise that was undone."),
        ("Governor", "MinVramFreeMiB") => ("0–8192 MiB", "Require this free VRAM before a planned distance raise."),
        ("Knobs", "EnableObjectDistance") => ("0, 1 (also false, true)", "Enable the object-distance hook path; automatic step changes await the governor."),
        ("Knobs", "MaxObjectDistance") => ("0–4", "Limit the planned object-distance step, with zero preserving the menu baseline."),
        ("Knobs", "EnableScatterDistance") => ("0, 1 (also false, true)", "Enable the checked grass-distance redirect, currently held at the game’s 90 m ceiling."),
        ("Knobs", "MaxScatterDistance") => ("0–4", "Limit the planned scatter step, further restricted by proven capacity headroom."),
        ("Knobs", "EnableTerrainDistance") => ("0, 1 (also false, true)", "Enable the terrain-distance hook path; automatic step changes await the governor."),
        ("Knobs", "MaxTerrainDistance") => ("0–4", "Limit the planned terrain-distance step, with zero preserving the menu baseline."),
        ("Knobs", "DecoDistanceScale") => ("0.5–1.0", "Select the parsed decoration threshold scale; startup application is not wired on this base."),
        ("Knobs", "DistantObjectsScale") => ("0.5–1.0", "Select the parsed distant-object threshold scale; startup application is not wired on this base."),
        ("Knobs", "TerrainRingScale") => ("1.0–1.6", "Select the parsed terrain-ring scale; startup application is not wired on this base."),
        ("Hdr", "PaperWhiteNits") => ("game, 40–400 nits", "Keep the game’s 250-nit paper white or apply a checked startup override."),
        ("Hdr", "FixPeak") => ("on, off", "Opt into HDR peak correction through the shared scene hook when a distance knob enables it."),
        ("Hdr", "DisplayPeakNits") => ("game, whole number 80–10000 nits", "Choose the game peak or panel target used only when FixPeak is on."),
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
