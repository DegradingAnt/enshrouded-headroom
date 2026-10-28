//! The settings file, `enshrouded-headroom.ini`.
//!
//! `parse` never fails and never panics. A value that is missing, not valid or out of range falls
//! back to its default or is clamped, and each such case adds one line to the returned warnings.
//! The caller logs the warnings. Section and key names are not case sensitive.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use configparser::ini::Ini;

/// Name of the settings file, next to the `.asi`.
pub const INI_FILE_NAME: &str = "enshrouded-headroom.ini";

/// The settings file that `write_default` writes when there is none. It parses without warnings
/// and gives the same `Config` as `Config::default()`.
pub const DEFAULT_INI: &str = include_str!("../enshrouded-headroom.default.ini");

/// `[General] LogLevel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

impl From<LogLevel> for log::LevelFilter {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Error => log::LevelFilter::Error,
            LogLevel::Warn => log::LevelFilter::Warn,
            LogLevel::Info => log::LevelFilter::Info,
            LogLevel::Debug => log::LevelFilter::Debug,
        }
    }
}

/// `[General] TextureBudget`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextureBudget {
    /// Raise the budget to suit the GPU.
    Auto,
    /// Leave the game's budget alone.
    Off,
    /// A fixed budget in GiB (2 to 8).
    Gib(f64),
}

impl TextureBudget {
    /// How the settings file spells this value.
    fn ini_text(self) -> String {
        match self {
            TextureBudget::Auto => "auto".to_owned(),
            TextureBudget::Off => "off".to_owned(),
            TextureBudget::Gib(gib) => gib.to_string(),
        }
    }
}

/// `[General] AsyncCompute`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsyncCompute {
    On,
    Game,
}

/// `[General] Priority`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// Raise the process priority after the first device is created.
    AboveNormal,
    /// Leave the current process priority unchanged.
    Normal,
}

/// `[General] Reflex`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReflexMode {
    /// Follow the saved menu value; an unknown value uses `Auto`.
    Game,
    /// Enable on NVIDIA when the driver lists the extension.
    Auto,
    /// Disable Reflex.
    Off,
    /// Enable Reflex without boost when supported.
    On,
    /// Enable Reflex with boost when supported.
    Boost,
}

/// `[General] AntiLag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AntiLagMode {
    /// Enable on AMD when the extension and feature are supported.
    Auto,
    /// Disable Anti-Lag.
    Off,
    /// Enable on any vendor that supports the extension and feature.
    On,
}

/// The `[General]` section.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct General {
    pub enabled: bool,
    pub texture_budget: TextureBudget,
    pub texture_reserve_gib: f64,
    pub async_compute: AsyncCompute,
    /// The priority change made after renderer creation.
    pub priority: Priority,
    pub reflex: ReflexMode,
    /// The requested AMD Anti-Lag mode.
    pub anti_lag: AntiLagMode,
    pub allow_unknown_build: bool,
    pub log_level: LogLevel,
}

/// The `[Governor]` section: the keys in spec section 6.4.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GovernorCfg {
    /// `0` turns the governor off; otherwise 30 to 360.
    pub target_fps: u32,
    pub window_seconds: f64,
    pub raise_below: f64,
    pub raise_hold_seconds: f64,
    pub settle_seconds: f64,
    /// Always above `raise_below`.
    pub lower_above: f64,
    pub lower_hold_seconds: f64,
    pub hitch_factor: f64,
    pub block_seconds: f64,
    pub min_vram_free_mib: u32,
}

/// The `[Knobs]` section. Each `max_*` is 0 to 4.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KnobsCfg {
    pub enable_object_distance: bool,
    pub max_object_distance: u8,
    pub enable_scatter_distance: bool,
    pub max_scatter_distance: u8,
    pub enable_terrain_distance: bool,
    pub max_terrain_distance: u8,
    pub deco_distance_scale: f64,
    pub distant_objects_scale: f64,
    pub terrain_ring_scale: f64,
}

/// `[Hdr] PaperWhiteNits`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PaperWhiteNits {
    /// Keep the game's hard-coded 250-nit value.
    Game,
    /// Replace it with a finite value from 40 through 400 nits.
    Nits(f32),
}

/// The `[Hdr]` section.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HdrCfg {
    /// The composite pass's paper-white scale.
    pub paper_white_nits: PaperWhiteNits,
    /// Correct the HDR tonemapper peak after scene settings are rebuilt.
    pub fix_peak: bool,
    /// Panel peak in nits; `None` follows the game's fresh value.
    pub display_peak_nits: Option<u16>,
}

/// The `[Debug]` section (design.md section 8). Only FrameStats is shipped; legacy controls remain
/// omitted and off unless the player adds them. A consumer honours debug controls only with
/// `LogLevel = debug`, and that check is the consumer's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DebugCfg {
    /// FrameStats: permit debug frame reports when the governor is disabled.
    pub frame_stats: bool,
    /// `ScatterProbe`: read the game's scatter GPU counters (design.md 7.2a), so the governor can
    /// log them.
    pub scatter_probe: bool,
    /// `ForceStepObjectDistance`: hold the object knob at this step (0 to 4); `None` when absent.
    pub force_step_object: Option<u8>,
    /// `ForceStepScatterDistance`: hold the grass knob at this step (0 to 4); `None` when absent.
    pub force_step_scatter: Option<u8>,
    /// `ForceStepTerrainDistance`: hold the terrain knob at this step (0 to 4); `None` when absent.
    pub force_step_terrain: Option<u8>,
}

/// A shadow setting that leaves the existing Ultra value alone unless requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShadowValue<T> {
    /// Keep the game's value.
    #[default]
    Game,
    /// Replace the Ultra constant at start-up.
    Override(T),
}

/// The optional `[Shadows]` start-up constants. All defaults are `game`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ShadowsCfg {
    /// Ultra ray budget, 65 to 256.
    pub point_light_rays: ShadowValue<u32>,
    /// Ultra static-light distance in metres, 101 to 400.
    pub point_light_static_distance: ShadowValue<f32>,
    /// Ultra atlas tile size, 224 or 256 pixels.
    pub point_light_tile: ShadowValue<u32>,
    /// Ultra sun-cascade resolution, 4096 pixels.
    pub sun_resolution: ShadowValue<u32>,
}

/// All settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    pub general: General,
    pub governor: GovernorCfg,
    pub knobs: KnobsCfg,
    pub hdr: HdrCfg,
    /// The `[Debug]` keys; all off by default.
    pub debug: DebugCfg,
    /// Optional Ultra shadow constants.
    pub shadows: ShadowsCfg,
}

/// The default settings. `parse` falls back to these values, and `DEFAULT_INI` repeats them (a test
/// checks that the two agree).
const BUILTIN: Config = Config {
    hdr: HdrCfg {
        paper_white_nits: PaperWhiteNits::Game,
        fix_peak: false,
        display_peak_nits: None,
    },
    general: General {
        enabled: true,
        texture_budget: TextureBudget::Auto,
        texture_reserve_gib: 11.0,
        async_compute: AsyncCompute::On,
        priority: Priority::AboveNormal,
        reflex: ReflexMode::Auto,
        anti_lag: AntiLagMode::Auto,
        allow_unknown_build: false,
        log_level: LogLevel::Info,
    },
    governor: GovernorCfg {
        target_fps: 75,
        window_seconds: 2.0,
        raise_below: 0.90,
        raise_hold_seconds: 1.0,
        settle_seconds: 1.0,
        lower_above: 0.97,
        lower_hold_seconds: 0.3,
        hitch_factor: 2.0,
        block_seconds: 20.0,
        min_vram_free_mib: 1024,
    },
    knobs: KnobsCfg {
        enable_object_distance: true,
        max_object_distance: 4,
        enable_scatter_distance: true,
        max_scatter_distance: 4,
        enable_terrain_distance: true,
        max_terrain_distance: 4,
        deco_distance_scale: 1.0,
        distant_objects_scale: 1.0,
        terrain_ring_scale: 1.0,
    },
    shadows: ShadowsCfg {
        point_light_rays: ShadowValue::Game,
        point_light_static_distance: ShadowValue::Game,
        point_light_tile: ShadowValue::Game,
        sun_resolution: ShadowValue::Game,
    },
    debug: DebugCfg {
        frame_stats: false,
        scatter_probe: false,
        force_step_object: None,
        force_step_scatter: None,
        force_step_terrain: None,
    },
};

impl Default for Config {
    fn default() -> Self {
        BUILTIN
    }
}

/// The sections this version knows, spelled as the spec spells them. A section that is not here is
/// unknown.
const SECTIONS: [&str; 6] = ["General", "Governor", "Knobs", "Hdr", "Shadows", "Debug"];

/// Parse the text of a settings file. The `Vec` holds one warning per problem found.
pub fn parse(text: &str) -> (Config, Vec<String>) {
    // Notepad can save UTF-8 with a byte order mark. It would hide the first `[`.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut ini = Ini::new();
    if let Err(error) = ini.read(text.to_owned()) {
        let warning = format!("the settings file cannot be read ({error}), using all defaults");
        return (BUILTIN, vec![warning]);
    }
    let mut r = Reader::new(ini);
    let d = BUILTIN;

    // Struct fields are evaluated in order, so warnings come out in file order of the spec.
    let general = General {
        enabled: r.boolean("General", "Enabled", d.general.enabled),
        texture_budget: r.texture_budget(d.general.texture_budget),
        texture_reserve_gib: r.float(
            "General",
            "TextureReserveGiB",
            d.general.texture_reserve_gib,
            4.0,
            16.0,
        ),
        async_compute: r.choice(
            "General",
            "AsyncCompute",
            d.general.async_compute,
            &[("on", AsyncCompute::On), ("game", AsyncCompute::Game)],
        ),
        priority: r.choice(
            "General",
            "Priority",
            d.general.priority,
            &[
                ("above_normal", Priority::AboveNormal),
                ("normal", Priority::Normal),
            ],
        ),
        reflex: r.choice(
            "General",
            "Reflex",
            d.general.reflex,
            &[
                ("game", ReflexMode::Game),
                ("auto", ReflexMode::Auto),
                ("off", ReflexMode::Off),
                ("on", ReflexMode::On),
                ("boost", ReflexMode::Boost),
            ],
        ),
        anti_lag: r.choice(
            "General",
            "AntiLag",
            d.general.anti_lag,
            &[
                ("auto", AntiLagMode::Auto),
                ("off", AntiLagMode::Off),
                ("on", AntiLagMode::On),
            ],
        ),
        allow_unknown_build: r.boolean(
            "General",
            "AllowUnknownBuild",
            d.general.allow_unknown_build,
        ),
        log_level: r.choice(
            "General",
            "LogLevel",
            d.general.log_level,
            &[
                ("error", LogLevel::Error),
                ("warn", LogLevel::Warn),
                ("info", LogLevel::Info),
                ("debug", LogLevel::Debug),
            ],
        ),
    };

    let g = d.governor;
    let mut governor = GovernorCfg {
        target_fps: r.int("Governor", "TargetFps", g.target_fps, 30, 360, true),
        window_seconds: r.float("Governor", "WindowSeconds", g.window_seconds, 0.5, 10.0),
        raise_below: r.float("Governor", "RaiseBelow", g.raise_below, 0.5, 0.99),
        raise_hold_seconds: r.float(
            "Governor",
            "RaiseHoldSeconds",
            g.raise_hold_seconds,
            0.1,
            10.0,
        ),
        settle_seconds: r.float("Governor", "SettleSeconds", g.settle_seconds, 0.1, 10.0),
        lower_above: r.float("Governor", "LowerAbove", g.lower_above, 0.8, 1.0),
        lower_hold_seconds: r.float(
            "Governor",
            "LowerHoldSeconds",
            g.lower_hold_seconds,
            0.1,
            10.0,
        ),
        hitch_factor: r.float("Governor", "HitchFactor", g.hitch_factor, 1.2, 5.0),
        block_seconds: r.float("Governor", "BlockSeconds", g.block_seconds, 1.0, 300.0),
        min_vram_free_mib: r.int(
            "Governor",
            "MinVramFreeMiB",
            g.min_vram_free_mib,
            0,
            8192,
            false,
        ),
    };
    if governor.lower_above <= governor.raise_below {
        r.warn(format!(
            "[Governor] LowerAbove ({}) must be above RaiseBelow ({}), using the defaults {} and {}",
            governor.lower_above, governor.raise_below, g.lower_above, g.raise_below
        ));
        governor.raise_below = g.raise_below;
        governor.lower_above = g.lower_above;
    }

    let k = d.knobs;
    let knobs = KnobsCfg {
        enable_object_distance: r.boolean(
            "Knobs",
            "EnableObjectDistance",
            k.enable_object_distance,
        ),
        max_object_distance: r.max_step("MaxObjectDistance", k.max_object_distance),
        enable_scatter_distance: r.boolean(
            "Knobs",
            "EnableScatterDistance",
            k.enable_scatter_distance,
        ),
        max_scatter_distance: r.max_step("MaxScatterDistance", k.max_scatter_distance),
        enable_terrain_distance: r.boolean(
            "Knobs",
            "EnableTerrainDistance",
            k.enable_terrain_distance,
        ),
        max_terrain_distance: r.max_step("MaxTerrainDistance", k.max_terrain_distance),
        deco_distance_scale: r.float(
            "Knobs",
            "DecoDistanceScale",
            k.deco_distance_scale,
            0.5,
            1.0,
        ),
        distant_objects_scale: r.float(
            "Knobs",
            "DistantObjectsScale",
            k.distant_objects_scale,
            0.5,
            1.0,
        ),
        terrain_ring_scale: r.float("Knobs", "TerrainRingScale", k.terrain_ring_scale, 1.0, 1.6),
    };

    let hdr = HdrCfg {
        paper_white_nits: r.paper_white_nits(),
        fix_peak: r.choice("Hdr", "FixPeak", false, &[("on", true), ("off", false)]),
        display_peak_nits: r.display_peak_nits(),
    };
    let debug = DebugCfg {
        frame_stats: r.boolean("Debug", "FrameStats", d.debug.frame_stats),
        scatter_probe: r.boolean("Debug", "ScatterProbe", d.debug.scatter_probe),
        force_step_object: r.force_step("ForceStepObjectDistance"),
        force_step_scatter: r.force_step("ForceStepScatterDistance"),
        force_step_terrain: r.force_step("ForceStepTerrainDistance"),
    };

    let shadows = ShadowsCfg {
        point_light_rays: r.shadow("PointLightRays", |v| {
            v.parse::<u32>().ok().filter(|n| (65..=256).contains(n))
        }),
        point_light_static_distance: r.shadow("PointLightStaticDistance", |v| {
            parse_number(v)
                .filter(|n| (101.0..=400.0).contains(n))
                .map(|n| n as f32)
        }),
        point_light_tile: r.shadow("PointLightTile", |v| {
            v.parse::<u32>().ok().filter(|n| matches!(n, 224 | 256))
        }),
        sun_resolution: r.shadow("SunResolution", |v| {
            v.parse::<u32>().ok().filter(|n| *n == 4096)
        }),
    };

    r.report_unknown(&SECTIONS);

    (
        Config {
            general,
            governor,
            knobs,
            hdr,
            debug,
            shadows,
        },
        r.0.into_warnings(),
    )
}

/// Read and parse `<dir>\enshrouded-headroom.ini`. `Ok(None)` means there is no such file; any
/// other failure to read it is an `Err`.
pub fn read_ini(dir: &Path) -> io::Result<Option<(Config, Vec<String>)>> {
    match std::fs::read(dir.join(INI_FILE_NAME)) {
        // Bytes that are not UTF-8 become U+FFFD instead of an error. A hand-edited file with one
        // odd character in a comment must not cost the player their settings.
        Ok(bytes) => Ok(Some(parse(&String::from_utf8_lossy(&bytes)))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Write `DEFAULT_INI` as `<dir>\enshrouded-headroom.ini`, creating `dir` first. An existing file
/// is an error, not overwritten.
pub fn write_default(dir: &Path) -> io::Result<()> {
    let path = dir.join(INI_FILE_NAME);
    create_new_file(dir, &path, |file| file.write_all(DEFAULT_INI.as_bytes()))
}

/// Create `path` (and `dir` before it) and fill it with `fill`. `create_new` makes sure a file that
/// appeared in the meantime is never overwritten. If `fill` fails, the file this call created is
/// removed: a half-written settings file would be read as it is, and never repaired.
fn create_new_file(
    dir: &Path,
    path: &Path,
    fill: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    if let Err(error) = fill(&mut file) {
        // Windows cannot delete a file that is still open.
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}

use emberkit::config::parse_number;
#[cfg(test)]
use emberkit::config::{shorten, MAX_ECHO_CHARS};
/// Headroom-specific typed readers over shared primitives.
struct Reader(emberkit::config::Reader);
impl std::ops::Deref for Reader {
    type Target = emberkit::config::Reader;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for Reader {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Reader {
    /// An optional shadow constant. Invalid values warn once and keep the game value.
    fn shadow<T>(&mut self, key: &str, parse: impl FnOnce(&str) -> Option<T>) -> ShadowValue<T> {
        let Some(text) = self.raw("Shadows", key) else {
            return ShadowValue::Game;
        };
        let value = text.trim();
        if value.eq_ignore_ascii_case("game") {
            return ShadowValue::Game;
        }
        match parse(value) {
            Some(value) => ShadowValue::Override(value),
            None => {
                self.not_valid("Shadows", key, &text, "a supported shadow override", "game");
                ShadowValue::Game
            }
        }
    }

    fn new(ini: Ini) -> Self {
        Self(emberkit::config::Reader::new(ini))
    }
    /// A `[Knobs] Max*` key: a step from 0 to 4.
    fn max_step(&mut self, key: &str, default: u8) -> u8 {
        let step = self.int("Knobs", key, u32::from(default), 0, 4, false);
        u8::try_from(step).unwrap_or(default)
    }
    /// A `[Debug] ForceStep*` key: `None` when absent, else a step from 0 to 4. A whole number
    /// outside that range is clamped with a warning. Text that is not a whole number warns and
    /// counts as absent.
    fn force_step(&mut self, key: &str) -> Option<u8> {
        let text = self.raw("Debug", key)?;
        let Some(number) = parse_number(&text).filter(|number| number.fract() == 0.0) else {
            self.not_valid("Debug", key, &text, "a whole number from 0 to 4", "none");
            return None;
        };
        // A float-to-integer `as` cast saturates, like in `int`.
        let value = number as i64;
        let kept = value.clamp(0, 4);
        if kept != value {
            self.warn(format!(
                "[Debug] {key}: {value} is outside 0..=4, using {kept}"
            ));
        }
        u8::try_from(kept).ok()
    }
    /// `auto`, `off` or a number of GiB from 2 to 8.
    fn texture_budget(&mut self, default: TextureBudget) -> TextureBudget {
        let Some(text) = self.raw("General", "TextureBudget") else {
            return default;
        };
        match text.to_ascii_lowercase().as_str() {
            "auto" => TextureBudget::Auto,
            "off" => TextureBudget::Off,
            _ => match parse_number(&text) {
                Some(gib) => {
                    TextureBudget::Gib(self.clamp("General", "TextureBudget", gib, 2.0, 8.0))
                }
                None => {
                    self.not_valid(
                        "General",
                        "TextureBudget",
                        &text,
                        "auto, off or a number of GiB",
                        &default.ini_text(),
                    );
                    default
                }
            },
        }
    }
    /// `game` or a finite number from 40 through 400 nits; invalid values use `game`.
    fn paper_white_nits(&mut self) -> PaperWhiteNits {
        let Some(text) = self.raw("Hdr", "PaperWhiteNits") else {
            return PaperWhiteNits::Game;
        };
        if text.eq_ignore_ascii_case("game") {
            return PaperWhiteNits::Game;
        }
        if let Some(nits) = parse_number(&text).filter(|nits| (40.0..=400.0).contains(nits)) {
            return PaperWhiteNits::Nits(nits as f32);
        }
        self.not_valid(
            "Hdr",
            "PaperWhiteNits",
            &text,
            "game or a finite number in 40..=400",
            "game",
        );
        PaperWhiteNits::Game
    }
    /// `game` or a whole number from 80 through 10000; invalid values use `game`.
    fn display_peak_nits(&mut self) -> Option<u16> {
        let text = self.raw("Hdr", "DisplayPeakNits")?;
        if text.eq_ignore_ascii_case("game") {
            return None;
        }
        if let Some(nits) = parse_number(&text)
            .filter(|nits| nits.fract() == 0.0 && (80.0..=10000.0).contains(nits))
        {
            return Some(nits as u16);
        }
        self.not_valid(
            "Hdr",
            "DisplayPeakNits",
            &text,
            "game or a whole number in 80..=10000",
            "game",
        );
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// `target/test-tmp/<name>`, emptied first.
    fn tempdir_in_target(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Parse a file that holds one key.
    fn one(section: &str, key: &str, value: &str) -> (Config, Vec<String>) {
        parse(&format!("[{section}]\n{key}={value}\n"))
    }

    #[test]
    fn shadows_parse_ranges_and_keep_game_defaults() {
        assert_eq!(parse("").0.shadows, ShadowsCfg::default());
        for (key, good, bad) in [
            (
                "PointLightRays",
                vec!["65", "128", "256"],
                vec!["64", "257", "65.5", "NaN"],
            ),
            (
                "PointLightStaticDistance",
                vec!["101", "250,5", "400"],
                vec!["0", "100", "401", "inf"],
            ),
            (
                "PointLightTile",
                vec!["224", "256"],
                vec!["192", "225", "256.5"],
            ),
            (
                "SunResolution",
                vec!["4096"],
                vec!["2048", "8192", "4096.5"],
            ),
        ] {
            for value in good {
                let (cfg, warnings) = one("Shadows", key, value);
                assert!(warnings.is_empty(), "{key}={value}: {warnings:?}");
                assert_ne!(cfg.shadows, ShadowsCfg::default(), "{key}={value}");
            }
            for value in bad {
                let (cfg, warnings) = one("Shadows", key, value);
                assert_eq!(cfg.shadows, ShadowsCfg::default(), "{key}={value}");
                assert_eq!(warnings.len(), 1, "{key}={value}: {warnings:?}");
                assert!(warnings[0].contains("default game"));
            }
            let (cfg, warnings) = one("sHaDoWs", key, " GaMe ");
            assert_eq!(cfg.shadows, ShadowsCfg::default());
            assert!(warnings.is_empty());
        }
        let (cfg, warnings) = parse("[Shadows]\nPointLightRays=128\nPointLightStaticDistance=250,5\nPointLightTile=224\nSunResolution=4096\n");
        assert!(warnings.is_empty());
        assert_eq!(cfg.shadows.point_light_rays, ShadowValue::Override(128));
        assert_eq!(
            cfg.shadows.point_light_static_distance,
            ShadowValue::Override(250.5)
        );
        assert_eq!(cfg.shadows.point_light_tile, ShadowValue::Override(224));
        assert_eq!(cfg.shadows.sun_resolution, ShadowValue::Override(4096));
    }

    /// Every value is inside its range, whatever the input was.
    fn assert_in_range(c: &Config) {
        let (g, v, k) = (&c.general, &c.governor, &c.knobs);
        assert!(v.target_fps == 0 || (30..=360).contains(&v.target_fps));
        assert!((4.0..=16.0).contains(&g.texture_reserve_gib));
        if let TextureBudget::Gib(x) = g.texture_budget {
            assert!((2.0..=8.0).contains(&x));
        }
        assert!((0.5..=10.0).contains(&v.window_seconds));
        assert!((0.5..=0.99).contains(&v.raise_below));
        assert!((0.8..=1.0).contains(&v.lower_above));
        assert!(v.lower_above > v.raise_below);
        for s in [v.raise_hold_seconds, v.settle_seconds, v.lower_hold_seconds] {
            assert!((0.1..=10.0).contains(&s));
        }
        assert!((1.2..=5.0).contains(&v.hitch_factor));
        assert!((1.0..=300.0).contains(&v.block_seconds));
        assert!(v.min_vram_free_mib <= 8192);
        for m in [
            k.max_object_distance,
            k.max_scatter_distance,
            k.max_terrain_distance,
        ] {
            assert!(m <= 4);
        }
        assert!((0.5..=1.0).contains(&k.deco_distance_scale));
        assert!((0.5..=1.0).contains(&k.distant_objects_scale));
        assert!((1.0..=1.6).contains(&k.terrain_ring_scale));
    }

    /// The numeric value of a key, by its `.ini` name, for the range tables.
    fn number(c: &Config, key: &str) -> f64 {
        match key {
            "TargetFps" => f64::from(c.governor.target_fps),
            "TextureReserveGiB" => c.general.texture_reserve_gib,
            "WindowSeconds" => c.governor.window_seconds,
            "RaiseBelow" => c.governor.raise_below,
            "RaiseHoldSeconds" => c.governor.raise_hold_seconds,
            "SettleSeconds" => c.governor.settle_seconds,
            "LowerAbove" => c.governor.lower_above,
            "LowerHoldSeconds" => c.governor.lower_hold_seconds,
            "HitchFactor" => c.governor.hitch_factor,
            "BlockSeconds" => c.governor.block_seconds,
            "MinVramFreeMiB" => f64::from(c.governor.min_vram_free_mib),
            "MaxObjectDistance" => f64::from(c.knobs.max_object_distance),
            "MaxScatterDistance" => f64::from(c.knobs.max_scatter_distance),
            "MaxTerrainDistance" => f64::from(c.knobs.max_terrain_distance),
            "DecoDistanceScale" => c.knobs.deco_distance_scale,
            "DistantObjectsScale" => c.knobs.distant_objects_scale,
            "TerrainRingScale" => c.knobs.terrain_ring_scale,
            _ => panic!("no such numeric key {key}"),
        }
    }

    // ---- Priority ----

    #[test]
    fn priority_defaults_to_above_normal() {
        let (cfg, warnings) = parse("");
        assert_eq!(cfg.general.priority, Priority::AboveNormal);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(Config::default().general.priority, Priority::AboveNormal);
    }

    #[test]
    fn priority_accepts_both_settings() {
        for (value, expected) in [
            ("above_normal", Priority::AboveNormal),
            ("normal", Priority::Normal),
        ] {
            let (cfg, warnings) = one("General", "Priority", value);
            assert_eq!(cfg.general.priority, expected, "{value}");
            assert!(warnings.is_empty(), "{warnings:?}");
        }
    }

    #[test]
    fn priority_bad_value_warns_and_uses_above_normal() {
        let (cfg, warnings) = one("General", "Priority", "high");
        assert_eq!(cfg.general.priority, Priority::AboveNormal);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("[General] Priority"), "{warnings:?}");
        assert!(
            warnings[0].contains("using the default above_normal"),
            "{warnings:?}"
        );
    }

    #[test]
    fn priority_accepts_upper_case() {
        for (value, expected) in [
            ("ABOVE_NORMAL", Priority::AboveNormal),
            ("NORMAL", Priority::Normal),
        ] {
            let (cfg, warnings) = one("GENERAL", "PRIORITY", value);
            assert_eq!(cfg.general.priority, expected, "{value}");
            assert!(warnings.is_empty(), "{warnings:?}");
        }
    }

    #[test]
    fn hdr_peak_settings_default_off_and_reject_invalid_targets() {
        let (cfg, warnings) = parse("");
        assert!(!cfg.hdr.fix_peak);
        assert_eq!(cfg.hdr.display_peak_nits, None);
        assert!(warnings.is_empty());
        for word in ["on", "ON"] {
            let (cfg, warnings) = parse(&format!("[HDR]\nFixPeak={word}\nDisplayPeakNits=800,0"));
            assert!(cfg.hdr.fix_peak);
            assert_eq!(cfg.hdr.display_peak_nits, Some(800));
            assert!(warnings.is_empty(), "{warnings:?}");
        }
        for text in ["game", "GaMe", "80", "10000", "800.0"] {
            let (_, warnings) = parse(&format!("[Hdr]\nFixPeak=off\nDisplayPeakNits={text}"));
            assert!(warnings.is_empty(), "{text}: {warnings:?}");
        }
        for text in [
            "79", "10001", "800.5", "NaN", "inf", "1e300", "", "bad", "-1",
        ] {
            let (cfg, warnings) = parse(&format!("[Hdr]\nDisplayPeakNits={text}"));
            assert_eq!(cfg.hdr.display_peak_nits, None, "{text}");
            assert_eq!(warnings.len(), 1, "{text}: {warnings:?}");
            assert!(warnings[0].contains("[Hdr] DisplayPeakNits"));
            assert!(warnings[0].contains("using the default game"));
        }
        let (cfg, warnings) = parse("[Hdr]\nFixPeak=maybe");
        assert!(!cfg.hdr.fix_peak);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("using the default off"));
    }

    #[test]
    fn hdr_paper_white_accepts_game_and_finite_nits_only() {
        for text in [
            "",
            "[Hdr]\nPaperWhiteNits=game",
            "[HDR]\nPAPERWHITENITS=GaMe",
        ] {
            let (cfg, warnings) = parse(text);
            assert_eq!(cfg.hdr.paper_white_nits, PaperWhiteNits::Game);
            assert!(warnings.is_empty(), "{warnings:?}");
        }
        for (text, want) in [("40", 40.0), ("80", 80.0), ("200,5", 200.5), ("400", 400.0)] {
            let (cfg, warnings) = parse(&format!("[Hdr]\nPaperWhiteNits={text}"));
            assert_eq!(cfg.hdr.paper_white_nits, PaperWhiteNits::Nits(want));
            assert!(warnings.is_empty(), "{warnings:?}");
        }
        for text in [
            "39.99", "400.01", "0", "-1", "NaN", "inf", "-inf", "1e300", "", "banana",
        ] {
            let (cfg, warnings) = parse(&format!("[Hdr]\nPaperWhiteNits={text}"));
            assert_eq!(cfg.hdr.paper_white_nits, PaperWhiteNits::Game, "{text}");
            assert_eq!(warnings.len(), 1, "{text}: {warnings:?}");
            assert!(warnings[0].contains("[Hdr] PaperWhiteNits"));
            assert!(warnings[0].contains("using the default game"));
        }
    }

    // ---- The four tests from the task brief ----

    #[test]
    fn defaults_match_spec() {
        let (c, w) = parse(DEFAULT_INI);
        assert!(w.is_empty());
        assert_eq!(c.governor.target_fps, 75);
        assert!(matches!(c.general.async_compute, AsyncCompute::On));
        assert!(matches!(c.general.reflex, ReflexMode::Auto));
        assert_eq!(c.general.anti_lag, AntiLagMode::Auto);
        assert_eq!(c.governor.raise_below, 0.90);
        assert_eq!(c.governor.lower_hold_seconds, 0.3);
        assert_eq!(c.governor.min_vram_free_mib, 1024);
        assert_eq!(c.knobs.max_scatter_distance, 4);
    }

    #[test]
    fn bad_values_fall_back_and_warn() {
        let (c, w) =
            parse("[Governor]\nRaiseBelow=0,85\nLowerAbove=banana\nTargetFps=9999\n[Nope]\nX=1\n");
        assert_eq!(c.governor.raise_below, 0.85); // decimal comma accepted
        assert_eq!(c.governor.lower_above, 0.97); // garbage -> default
        assert_eq!(c.governor.target_fps, 360); // clamped
        assert!(w.len() >= 3); // garbage, clamp, unknown section
    }

    #[test]
    fn inverted_thresholds_reset_both() {
        let (c, w) = parse("[Governor]\nRaiseBelow=0.98\nLowerAbove=0.9\n");
        assert_eq!(
            (c.governor.raise_below, c.governor.lower_above),
            (0.90, 0.97)
        );
        assert!(!w.is_empty());
    }

    #[test]
    fn missing_file_writes_default() {
        let d = tempdir_in_target("cfg_missing");
        assert_eq!(read_ini(&d).unwrap(), None);
        write_default(&d).unwrap();
        let (c, w) = read_ini(&d).unwrap().unwrap();
        assert_eq!(c, Config::default());
        assert!(w.is_empty());
        assert_eq!(
            std::fs::read_to_string(d.join("enshrouded-headroom.ini")).unwrap(),
            DEFAULT_INI
        );
    }

    // ---- Defaults ----

    #[test]
    fn default_ini_agrees_with_builtin_defaults() {
        let (parsed, w) = parse(DEFAULT_INI);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn every_default_matches_the_spec_table() {
        let c = Config::default();
        let g = c.general;
        assert!(g.enabled);
        assert_eq!(g.texture_budget, TextureBudget::Auto);
        assert_eq!(g.texture_reserve_gib, 11.0);
        assert_eq!(g.async_compute, AsyncCompute::On);
        assert_eq!(g.priority, Priority::AboveNormal);
        assert_eq!(g.reflex, ReflexMode::Auto);
        assert_eq!(g.anti_lag, AntiLagMode::Auto);
        assert!(!g.allow_unknown_build);
        assert_eq!(g.log_level, LogLevel::Info);
        let v = c.governor;
        assert_eq!(v.target_fps, 75);
        assert_eq!(v.window_seconds, 2.0);
        assert_eq!(v.raise_below, 0.90);
        assert_eq!(v.raise_hold_seconds, 1.0);
        assert_eq!(v.settle_seconds, 1.0);
        assert_eq!(v.lower_above, 0.97);
        assert_eq!(v.lower_hold_seconds, 0.3);
        assert_eq!(v.hitch_factor, 2.0);
        assert_eq!(v.block_seconds, 20.0);
        assert_eq!(v.min_vram_free_mib, 1024);
        let k = c.knobs;
        assert!(k.enable_object_distance && k.enable_scatter_distance && k.enable_terrain_distance);
        assert_eq!(
            (
                k.max_object_distance,
                k.max_scatter_distance,
                k.max_terrain_distance
            ),
            (4, 4, 4)
        );
        assert_eq!(
            (
                k.deco_distance_scale,
                k.distant_objects_scale,
                k.terrain_ring_scale
            ),
            (1.0, 1.0, 1.0)
        );
    }

    #[test]
    fn empty_file_gives_defaults_without_warnings() {
        let (c, w) = parse("");
        assert_eq!(c, Config::default());
        assert!(w.is_empty());
    }

    #[test]
    fn default_ini_starts_with_why_not_rust_line() {
        assert!(DEFAULT_INI.starts_with("; WHY NOT RUST:"));
    }

    // ---- Ranges ----

    /// (section, key, lowest allowed, highest allowed)
    const RANGES: &[(&str, &str, f64, f64)] = &[
        ("General", "TextureReserveGiB", 4.0, 16.0),
        ("Governor", "WindowSeconds", 0.5, 10.0),
        ("Governor", "RaiseHoldSeconds", 0.1, 10.0),
        ("Governor", "SettleSeconds", 0.1, 10.0),
        ("Governor", "LowerHoldSeconds", 0.1, 10.0),
        ("Governor", "HitchFactor", 1.2, 5.0),
        ("Governor", "BlockSeconds", 1.0, 300.0),
        ("Governor", "MinVramFreeMiB", 0.0, 8192.0),
        ("Knobs", "MaxObjectDistance", 0.0, 4.0),
        ("Knobs", "MaxScatterDistance", 0.0, 4.0),
        ("Knobs", "MaxTerrainDistance", 0.0, 4.0),
        ("Knobs", "DecoDistanceScale", 0.5, 1.0),
        ("Knobs", "DistantObjectsScale", 0.5, 1.0),
        ("Knobs", "TerrainRingScale", 1.0, 1.6),
    ];

    #[test]
    fn values_on_the_range_edges_are_kept_without_warnings() {
        for &(section, key, lo, hi) in RANGES {
            for edge in [lo, hi] {
                let (c, w) = one(section, key, &edge.to_string());
                assert!(w.is_empty(), "{key}={edge}: {w:?}");
                assert_eq!(number(&c, key), edge, "{key}");
            }
        }
    }

    #[test]
    fn values_outside_the_range_are_clamped_with_one_warning() {
        for &(section, key, lo, hi) in RANGES {
            for (given, expected) in [(lo - 1.0, lo), (hi + 1.0, hi)] {
                let (c, w) = one(section, key, &given.to_string());
                assert_eq!(number(&c, key), expected, "{key}={given}");
                assert_eq!(w.len(), 1, "{key}={given}: {w:?}");
                assert!(w[0].contains(key), "warning names the key: {}", w[0]);
            }
        }
    }

    #[test]
    fn target_fps_is_zero_or_30_to_360() {
        for (given, expected, warns) in [
            ("0", 0, false),
            ("30", 30, false),
            ("75", 75, false),
            ("360", 360, false),
            ("1", 30, true),
            ("29", 30, true),
            ("-5", 30, true),
            ("361", 360, true),
            ("9999", 360, true),
        ] {
            let (c, w) = one("Governor", "TargetFps", given);
            assert_eq!(c.governor.target_fps, expected, "TargetFps={given}");
            assert_eq!(!w.is_empty(), warns, "TargetFps={given}: {w:?}");
        }
    }

    #[test]
    fn target_fps_is_a_governor_key_only() {
        // Spec 6.4 and 8: TargetFps lives in [Governor].
        let (c, w) = one("Governor", "TargetFps", "144");
        assert_eq!(c.governor.target_fps, 144);
        assert!(w.is_empty(), "{w:?}");
        // In [General] it is an unknown key: ignored, with a warning.
        let (c, w) = one("General", "TargetFps", "144");
        assert_eq!(c.governor.target_fps, 75);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].to_lowercase().contains("targetfps"), "{}", w[0]);
    }

    #[test]
    fn threshold_ranges() {
        // RaiseBelow 0.5..=0.99 and LowerAbove 0.8..=1.0, with LowerAbove above RaiseBelow.
        let (c, w) = parse("[Governor]\nRaiseBelow=0.5\nLowerAbove=0.8\n");
        assert_eq!((c.governor.raise_below, c.governor.lower_above), (0.5, 0.8));
        assert!(w.is_empty(), "{w:?}");
        let (c, w) = parse("[Governor]\nRaiseBelow=0.99\nLowerAbove=1.0\n");
        assert_eq!(
            (c.governor.raise_below, c.governor.lower_above),
            (0.99, 1.0)
        );
        assert!(w.is_empty(), "{w:?}");
        // Too low: clamped.
        let (c, w) = parse("[Governor]\nRaiseBelow=0.1\nLowerAbove=0.1\n");
        assert_eq!((c.governor.raise_below, c.governor.lower_above), (0.5, 0.8));
        assert_eq!(w.len(), 2, "{w:?}");
        // Too high: clamped.
        let (c, w) = parse("[Governor]\nRaiseBelow=5\nLowerAbove=5\n");
        assert_eq!(
            (c.governor.raise_below, c.governor.lower_above),
            (0.99, 1.0)
        );
        assert_eq!(w.len(), 2, "{w:?}");
    }

    #[test]
    fn lower_above_must_be_strictly_above_raise_below() {
        let (c, w) = parse("[Governor]\nRaiseBelow=0.9\nLowerAbove=0.9\n");
        assert_eq!(
            (c.governor.raise_below, c.governor.lower_above),
            (0.90, 0.97)
        );
        assert_eq!(w.len(), 1, "{w:?}");
        // A lone RaiseBelow above the default LowerAbove is the same case.
        let (c, w) = parse("[Governor]\nRaiseBelow=0.99\n");
        assert_eq!(
            (c.governor.raise_below, c.governor.lower_above),
            (0.90, 0.97)
        );
        assert_eq!(w.len(), 1, "{w:?}");
        // A clamp can create the inversion: LowerAbove 0.1 becomes 0.8, which is not above 0.9.
        let (c, w) = parse("[Governor]\nLowerAbove=0.1\n");
        assert_eq!(
            (c.governor.raise_below, c.governor.lower_above),
            (0.90, 0.97)
        );
        assert_eq!(w.len(), 2, "{w:?}");
    }

    // ---- Value forms ----

    #[test]
    fn texture_budget_forms() {
        for (given, expected) in [
            ("auto", TextureBudget::Auto),
            ("AUTO", TextureBudget::Auto),
            ("off", TextureBudget::Off),
            (" Off ", TextureBudget::Off),
            ("6", TextureBudget::Gib(6.0)),
            ("6.5", TextureBudget::Gib(6.5)),
            ("6,5", TextureBudget::Gib(6.5)),
            ("2", TextureBudget::Gib(2.0)),
            ("8", TextureBudget::Gib(8.0)),
        ] {
            let (c, w) = one("General", "TextureBudget", given);
            assert_eq!(c.general.texture_budget, expected, "TextureBudget={given}");
            assert!(w.is_empty(), "TextureBudget={given}: {w:?}");
        }
        for (given, expected) in [("1", 2.0), ("0", 2.0), ("9", 8.0), ("100", 8.0)] {
            let (c, w) = one("General", "TextureBudget", given);
            assert_eq!(c.general.texture_budget, TextureBudget::Gib(expected));
            assert_eq!(w.len(), 1, "TextureBudget={given}: {w:?}");
        }
        let (c, w) = one("General", "TextureBudget", "lots");
        assert_eq!(c.general.texture_budget, TextureBudget::Auto);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("auto"), "warning names the default: {}", w[0]);
    }

    #[test]
    fn enum_values_are_read_without_regard_to_case() {
        let (c, w) = parse(
            "[General]\nAsyncCompute=Game\nReflex=BOOST\nLogLevel=debug\n\
             [Knobs]\nEnableObjectDistance=0\n",
        );
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.general.async_compute, AsyncCompute::Game);
        assert_eq!(c.general.reflex, ReflexMode::Boost);
        assert_eq!(c.general.log_level, LogLevel::Debug);
        assert!(!c.knobs.enable_object_distance);
        for (given, expected) in [
            ("game", ReflexMode::Game),
            ("off", ReflexMode::Off),
            ("on", ReflexMode::On),
            ("boost", ReflexMode::Boost),
        ] {
            assert_eq!(one("General", "Reflex", given).0.general.reflex, expected);
        }
        for (given, expected) in [
            ("error", LogLevel::Error),
            ("warn", LogLevel::Warn),
            ("info", LogLevel::Info),
            ("debug", LogLevel::Debug),
        ] {
            assert_eq!(
                one("General", "LogLevel", given).0.general.log_level,
                expected
            );
        }
    }

    /// W19 latency choices share the parser's default and warning contract.
    #[test]
    fn latency_choices_defaults_and_bad_values() {
        for (given, expected) in [
            ("auto", ReflexMode::Auto),
            ("AUTO", ReflexMode::Auto),
            ("game", ReflexMode::Game),
            ("off", ReflexMode::Off),
            ("on", ReflexMode::On),
            ("boost", ReflexMode::Boost),
        ] {
            let (c, w) = one("General", "Reflex", given);
            assert_eq!(c.general.reflex, expected);
            assert!(w.is_empty(), "{w:?}");
        }
        for (given, expected) in [
            ("auto", AntiLagMode::Auto),
            ("AUTO", AntiLagMode::Auto),
            ("off", AntiLagMode::Off),
            ("on", AntiLagMode::On),
        ] {
            let (c, w) = one("General", "AntiLag", given);
            assert_eq!(c.general.anti_lag, expected);
            assert!(w.is_empty(), "{w:?}");
        }
        for key in ["Reflex", "AntiLag"] {
            for given in ["", "turbo", "1", "boosted"] {
                let (c, w) = one("General", key, given);
                assert_eq!(c, Config::default());
                assert_eq!(w.len(), 1, "{key}={given}: {w:?}");
                assert!(w[0].contains(key) && w[0].contains("default auto"));
            }
        }
        assert_eq!(
            DEFAULT_INI
                .lines()
                .filter(|line| !line.starts_with(';') && line.contains('='))
                .count(),
            36
        );
    }

    #[test]
    fn booleans_accept_0_1_true_false() {
        for (given, expected) in [
            ("1", true),
            ("0", false),
            ("true", true),
            ("false", false),
            ("TRUE", true),
            ("False", false),
        ] {
            let (c, w) = one("General", "Enabled", given);
            assert_eq!(c.general.enabled, expected, "Enabled={given}");
            assert!(w.is_empty(), "Enabled={given}: {w:?}");
        }
        let (c, w) = one("General", "AllowUnknownBuild", "1");
        assert!(c.general.allow_unknown_build);
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn log_level_maps_to_level_filter() {
        assert_eq!(
            log::LevelFilter::from(LogLevel::Error),
            log::LevelFilter::Error
        );
        assert_eq!(
            log::LevelFilter::from(LogLevel::Warn),
            log::LevelFilter::Warn
        );
        assert_eq!(
            log::LevelFilter::from(LogLevel::Info),
            log::LevelFilter::Info
        );
        assert_eq!(
            log::LevelFilter::from(LogLevel::Debug),
            log::LevelFilter::Debug
        );
    }

    #[test]
    fn not_valid_values_use_the_default_with_one_warning_that_names_the_key() {
        for (section, key, given) in [
            ("General", "Enabled", "maybe"),
            ("General", "AllowUnknownBuild", "2"),
            ("General", "AsyncCompute", "off"),
            ("General", "Reflex", "turbo"),
            ("General", "LogLevel", "verbose"),
            ("General", "TextureReserveGiB", "lots"),
            ("Governor", "TargetFps", "fast"),
            ("Governor", "TargetFps", "75.5"),
            ("Governor", "WindowSeconds", "nan"),
            ("Governor", "HitchFactor", "inf"),
            ("Governor", "BlockSeconds", "-inf"),
            ("Governor", "MinVramFreeMiB", "100.5"),
            ("Knobs", "MaxObjectDistance", "2,5"),
            ("Knobs", "MaxObjectDistance", "many"),
            ("Knobs", "EnableScatterDistance", "yes"),
            ("Knobs", "TerrainRingScale", "1.0.0"),
            ("Governor", "SettleSeconds", ""),
        ] {
            let (c, w) = one(section, key, given);
            assert_eq!(c, Config::default(), "{key}={given}");
            assert_eq!(w.len(), 1, "{key}={given}: {w:?}");
            assert!(w[0].contains(key), "warning names the key: {}", w[0]);
        }
        // A key with no `=` at all is an empty value.
        let (c, w) = parse("[Governor]\nHitchFactor\n");
        assert_eq!(c, Config::default());
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("HitchFactor"));
    }

    // ---- File structure ----

    #[test]
    fn unknown_sections_and_keys_warn_once_each() {
        let (c, w) = parse(
            "[General]\nColour=blue\nEnabled=1\n[Nope]\nX=1\nY=2\n[Knobs]\nMaxObjectDistance=3\nZed=1\n",
        );
        assert_eq!(c.knobs.max_object_distance, 3);
        // One warning each for Colour, [Nope] (once, not for X and Y) and Zed.
        assert_eq!(w.len(), 3, "{w:?}");
        // configparser lower-cases names it does not know, so match without regard to case.
        assert!(w.iter().any(|m| m.to_lowercase().contains("nope")));
        assert!(w.iter().any(|m| m.to_lowercase().contains("colour")));
        assert!(w.iter().any(|m| m.to_lowercase().contains("zed")));
        assert!(!w.iter().any(|m| m.contains("MaxObjectDistance")));
    }

    #[test]
    fn keys_before_any_section_are_unknown() {
        let (c, w) = parse("TargetFps=100\n[General]\nEnabled=0\n");
        assert_eq!(c.governor.target_fps, 75);
        assert!(!c.general.enabled);
        assert_eq!(w.len(), 1, "{w:?}");
    }

    #[test]
    fn section_and_key_names_are_not_case_sensitive() {
        let (c, w) = parse("[GOVERNOR]\nraisebelow=0.8\n[knobs]\nMAXOBJECTDISTANCE=2\n");
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.governor.raise_below, 0.8);
        assert_eq!(c.knobs.max_object_distance, 2);
    }

    #[test]
    fn comments_blank_lines_and_spaces_are_ignored() {
        let (c, w) = parse(
            "; a comment\n# another\n\n  [Governor]  \n  RaiseBelow = 0.85   ; note\n\
             \tHitchFactor:3\r\nBlockSeconds=30 # note\r\n",
        );
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.governor.raise_below, 0.85);
        assert_eq!(c.governor.hitch_factor, 3.0);
        assert_eq!(c.governor.block_seconds, 30.0);
    }

    #[test]
    fn inline_comments_and_spaces_are_ignored_for_every_value_type() {
        // int, bool, choice, TextureBudget and float keys, each with spaces and an inline comment.
        let (c, w) = parse(
            "[General]\n\
             Enabled = 0 # note\n\
             Reflex = on ; note\n\
             AsyncCompute =  game   # note\n\
             TextureBudget = 6 ; note\n\
             LogLevel\t=\twarn\t; note\n\
             [Governor]\n\
             TargetFps = 60 ; note\n\
             MinVramFreeMiB  =  2048   # note\n\
             HitchFactor = 3 ; note\n\
             [Knobs]\n\
             MaxObjectDistance = 2 # note\n\
             EnableTerrainDistance = false ; note\n",
        );
        assert!(w.is_empty(), "{w:?}");
        assert!(!c.general.enabled);
        assert_eq!(c.general.reflex, ReflexMode::On);
        assert_eq!(c.general.async_compute, AsyncCompute::Game);
        assert_eq!(c.general.texture_budget, TextureBudget::Gib(6.0));
        assert_eq!(c.general.log_level, LogLevel::Warn);
        assert_eq!(c.governor.target_fps, 60);
        assert_eq!(c.governor.min_vram_free_mib, 2048);
        assert_eq!(c.governor.hitch_factor, 3.0);
        assert_eq!(c.knobs.max_object_distance, 2);
        assert!(!c.knobs.enable_terrain_distance);
    }

    #[test]
    fn a_leading_byte_order_mark_is_ignored() {
        let (c, w) = parse("\u{feff}[General]\nEnabled=0\n");
        assert!(w.is_empty(), "{w:?}");
        assert!(!c.general.enabled);
        // Also for the shipped default file saved by an editor that adds one.
        let with_bom = format!("\u{feff}{DEFAULT_INI}");
        assert_eq!(parse(&with_bom), parse(DEFAULT_INI));
    }

    #[test]
    fn a_file_that_cannot_be_parsed_gives_the_defaults_and_a_warning() {
        for text in ["[General\nEnabled=0\n", "[Governor]\n=5\n"] {
            let (c, w) = parse(text);
            assert_eq!(c, Config::default(), "{text:?}");
            assert_eq!(w.len(), 1, "{text:?}: {w:?}");
        }
    }

    #[test]
    fn hostile_input_never_panics_and_always_gives_a_valid_config() {
        let long_key = format!("[Governor]\n{}=1\n", "k".repeat(100_000));
        let long_value = format!("[Governor]\nRaiseBelow={}\n", "9".repeat(100_000));
        let many_sections = "[a]\n".repeat(10_000);
        let long_section = format!("[{}]\nX=1\n", "s".repeat(100_000));
        // Multi-byte characters, so that a cut at 64 bytes would fall inside one.
        let long_unknown_key = format!("[Knobs]\n{}=1\n", "é".repeat(1_000));
        let long_bad_bool = format!("[General]\nEnabled={}\n", "é".repeat(1_000));
        let long_bad_choice = format!("[General]\nReflex={}\n", "😀".repeat(1_000));
        let long_bad_budget = format!("[General]\nTextureBudget={}\n", "ß".repeat(1_000));
        let long_bad_int = format!("[Governor]\nTargetFps={}.5\n", "7".repeat(1_000));
        let inputs = [
            // 301 digits when printed.
            "[Governor]\nHitchFactor=1e300\n",
            "[Governor]\nHitchFactor=-1e300\n",
            "[Governor]\nTargetFps=1e300\n",
            "[Governor]\nTargetFps=-1e300\n",
            "",
            "[",
            "]",
            "[]",
            "[[[",
            "]]][[[",
            "=",
            "==",
            ":",
            "[General]\n=1",
            "[General",
            "\u{0}",
            "\u{feff}",
            "\u{feff}\u{feff}[General]",
            ";",
            "#",
            "[General]\nLogLevel=\u{ff}\u{fffd}\n",
            "[General]\nTargetFps=\u{0}\n",
            "[General]\nTargetFps=99999999999999999999999\n",
            "[Governor]\nRaiseBelow=1e400\n",
            "[Governor]\nRaiseBelow=-1e400\n",
            "[Governor]\nRaiseBelow=1,2,3\n",
            "[Governor]\nRaiseBelow=0,\n",
            "[Governor]\nRaiseBelow=,\n",
            "[Governor]\nLowerAbove=nan\nRaiseBelow=nan\n",
            "[Knobs]\nMaxObjectDistance=-9223372036854775808\n",
            "[Knobs]\nMaxObjectDistance=9223372036854775807\n",
            "[\u{1F600}]\n\u{1F600}=\u{1F600}\n",
            "[General]\r\r\nEnabled=1\r\n\r",
            &long_key,
            &long_value,
            &many_sections,
            &long_section,
            &long_unknown_key,
            &long_bad_bool,
            &long_bad_choice,
            &long_bad_budget,
            &long_bad_int,
        ];
        for text in inputs {
            let (c, w) = parse(text);
            assert_in_range(&c);
            // A warning repeats at most 64 characters of the player's text, plus the fixed words.
            for warning in &w {
                assert!(
                    warning.chars().count() <= MAX_WARNING_CHARS,
                    "{} characters: {warning}",
                    warning.chars().count()
                );
            }
        }
    }

    /// No warning is longer than this, however long the text it is about. The longest fixed text is
    /// about 100 characters, and the player's own text adds at most `MAX_ECHO_CHARS` + 1.
    const MAX_WARNING_CHARS: usize = 200;

    #[test]
    fn shorten_cuts_on_a_character_boundary() {
        assert_eq!(shorten(""), "");
        assert_eq!(shorten("short"), "short");
        let exact = "a".repeat(MAX_ECHO_CHARS);
        assert_eq!(shorten(&exact), exact);
        assert_eq!(shorten(&format!("{exact}b")), format!("{exact}…"));
        // 2-byte and 4-byte characters: the cut is by character, not by byte.
        let long = "é".repeat(MAX_ECHO_CHARS + 10);
        assert_eq!(shorten(&long), format!("{}…", "é".repeat(MAX_ECHO_CHARS)));
        let long = "😀".repeat(MAX_ECHO_CHARS + 10);
        assert_eq!(shorten(&long), format!("{}…", "😀".repeat(MAX_ECHO_CHARS)));
    }

    #[test]
    fn long_values_and_keys_are_cut_in_the_warning() {
        let value = "x".repeat(500);
        let (_, w) = one("General", "Reflex", &value);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].contains(&format!("{}…", "x".repeat(MAX_ECHO_CHARS))),
            "{}",
            w[0]
        );
        assert!(!w[0].contains(&"x".repeat(MAX_ECHO_CHARS + 1)), "{}", w[0]);
        let (_, w) = parse(&format!("[Knobs]\n{}=1\n", "k".repeat(500)));
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].contains(&format!("{}…", "k".repeat(MAX_ECHO_CHARS))),
            "{}",
            w[0]
        );
        assert!(!w[0].contains(&"k".repeat(MAX_ECHO_CHARS + 1)), "{}", w[0]);
        // A number prints at full length (1e300 is 301 digits) but the warning shows 64 of them.
        let (c, w) = one("Governor", "HitchFactor", "1e300");
        assert_eq!(c.governor.hitch_factor, 5.0);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains('…'), "{}", w[0]);
    }

    #[test]
    fn integer_keys_accept_a_number_with_a_zero_fraction() {
        for (given, expected) in [
            ("60.0", 60),
            ("60,0", 60),
            ("144.00", 144),
            ("3e2", 300),
            ("0.0", 0),
            ("0,0", 0),
            ("-0", 0),
            ("+75", 75),
        ] {
            let (c, w) = one("Governor", "TargetFps", given);
            assert_eq!(c.governor.target_fps, expected, "TargetFps={given}");
            assert!(w.is_empty(), "TargetFps={given}: {w:?}");
        }
        // 0.0 turns the governor off, the same as 0.
        let (c, w) = one("Governor", "TargetFps", "0.0");
        assert_eq!(c.governor.target_fps, 0);
        assert!(w.is_empty(), "{w:?}");
        for (key, given, expected) in [
            ("MaxObjectDistance", "4,0", 4),
            ("MaxScatterDistance", "2.0", 2),
            ("MaxTerrainDistance", "0.0", 0),
        ] {
            let (c, w) = one("Knobs", key, given);
            assert_eq!(number(&c, key), f64::from(expected), "{key}={given}");
            assert!(w.is_empty(), "{key}={given}: {w:?}");
        }
        let (c, w) = one("Governor", "MinVramFreeMiB", "1e3");
        assert_eq!(c.governor.min_vram_free_mib, 1000);
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn integer_keys_still_clamp_and_still_reject_a_fraction() {
        // Not whole: not valid, so the default, with one warning.
        for (section, key, given) in [
            ("Governor", "TargetFps", "60.5"),
            ("Governor", "TargetFps", "60,5"),
            ("Governor", "TargetFps", "0.5"),
            ("Governor", "MinVramFreeMiB", "1024.001"),
            ("Knobs", "MaxScatterDistance", "3.5"),
        ] {
            let (c, w) = one(section, key, given);
            assert_eq!(c, Config::default(), "{key}={given}");
            assert_eq!(w.len(), 1, "{key}={given}: {w:?}");
        }
        // Whole but out of range: clamped, with one warning (also above the old i64 limit).
        for (given, expected) in [
            ("20.0", 30),
            ("400,0", 360),
            ("99999999999999999999", 360),
            ("-99999999999999999999", 30),
        ] {
            let (c, w) = one("Governor", "TargetFps", given);
            assert_eq!(c.governor.target_fps, expected, "TargetFps={given}");
            assert_eq!(w.len(), 1, "TargetFps={given}: {w:?}");
        }
        let (c, w) = one("Governor", "MinVramFreeMiB", "99999999999999999999");
        assert_eq!(c.governor.min_vram_free_mib, 8192);
        assert_eq!(w.len(), 1, "{w:?}");
    }

    #[test]
    fn texture_budget_warning_names_the_default_it_was_given() {
        let reader = |text: &str| {
            let mut ini = Ini::new();
            ini.read(format!("[General]\nTextureBudget={text}\n"))
                .unwrap();
            Reader::new(ini)
        };
        for (default, named) in [
            (TextureBudget::Auto, "default auto"),
            (TextureBudget::Off, "default off"),
            (TextureBudget::Gib(6.5), "default 6.5"),
        ] {
            let mut r = reader("lots");
            assert_eq!(r.texture_budget(default), default);
            assert_eq!(r.warnings().len(), 1, "{:?}", r.warnings());
            assert!(r.warnings()[0].contains(named), "{}", r.warnings()[0]);
        }
    }

    // ---- Settings file reads and writes ----

    #[test]
    fn existing_file_is_read_and_left_alone() {
        let d = tempdir_in_target("cfg_existing");
        let path = d.join(INI_FILE_NAME);
        let text = "[Governor]\nTargetFps=100\n[Knobs]\nMaxTerrainDistance=1\n";
        std::fs::write(&path, text).unwrap();
        let (c, w) = read_ini(&d).unwrap().unwrap();
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.governor.target_fps, 100);
        assert_eq!(c.knobs.max_terrain_distance, 1);
        assert_eq!(
            write_default(&d).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn file_that_is_not_utf8_does_not_panic() {
        let d = tempdir_in_target("cfg_not_utf8");
        std::fs::write(
            d.join(INI_FILE_NAME),
            b"[Governor]\nTargetFps=60\n; caf\xe9 \xff\xfe\n",
        )
        .unwrap();
        let (c, w) = read_ini(&d).unwrap().unwrap();
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.governor.target_fps, 60);
    }

    #[test]
    fn missing_folder_is_created_for_the_default_file() {
        let d = tempdir_in_target("cfg_missing_folder").join("a").join("b");
        write_default(&d).unwrap();
        let (c, w) = read_ini(&d).unwrap().unwrap();
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c, Config::default());
        assert_eq!(
            std::fs::read_to_string(d.join(INI_FILE_NAME)).unwrap(),
            DEFAULT_INI
        );
    }

    #[test]
    fn a_failed_write_removes_the_file_this_call_created() {
        let d = tempdir_in_target("cfg_failed_write");
        let path = d.join(INI_FILE_NAME);
        // The real creation and clean-up code runs; only the write itself is made to fail, half
        // way, the way a full disk would.
        let result = create_new_file(&d, &path, |file| {
            file.write_all(b"[General]\nEna")?;
            Err(io::Error::other("disk full"))
        });
        assert_eq!(result.unwrap_err().to_string(), "disk full");
        assert!(!path.exists(), "the half-written file must be gone");
        // So the next start finds no file and writes a whole one.
        assert_eq!(read_ini(&d).unwrap(), None);
        write_default(&d).unwrap();
        let (c, w) = read_ini(&d).unwrap().unwrap();
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c, Config::default());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), DEFAULT_INI);
    }

    #[test]
    fn a_failed_write_never_removes_a_file_that_was_already_there() {
        let d = tempdir_in_target("cfg_failed_write_existing");
        let path = d.join(INI_FILE_NAME);
        std::fs::write(&path, "[Governor]\nTargetFps=100\n").unwrap();
        let mut fill_ran = false;
        let result = create_new_file(&d, &path, |_| {
            fill_ran = true;
            Ok(())
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert!(!fill_ran, "an existing file is not written to");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[Governor]\nTargetFps=100\n"
        );
    }

    #[test]
    fn invalid_choices_name_the_default_that_is_used_in_the_warning() {
        for (key, default) in [
            ("AsyncCompute", "on"),
            ("Reflex", "auto"),
            ("AntiLag", "auto"),
            ("LogLevel", "info"),
        ] {
            let (config, warnings) = one("General", key, "invalid");
            assert_eq!(config, Config::default());
            assert_eq!(warnings.len(), 1);
            assert!(
                warnings[0].ends_with(&format!("using the default {default}")),
                "{key}: {warnings:?}"
            );
        }
    }

    // ---- [Debug] (plan Task 13b) ----

    #[test]
    fn debug_keys_are_read_and_a_step_out_of_range_is_clamped_with_one_warning() {
        let (c, w) = parse("[Debug]\nScatterProbe=1\nForceStepScatterDistance=7\n");
        assert_eq!(
            c.debug,
            DebugCfg {
                frame_stats: false,
                scatter_probe: true,
                force_step_object: None,
                force_step_scatter: Some(4),
                force_step_terrain: None,
            }
        );
        assert_eq!(
            w,
            ["[Debug] ForceStepScatterDistance: 7 is outside 0..=4, using 4"],
            "{w:?}"
        );
    }

    #[test]
    fn the_default_file_ships_only_frame_stats_and_gives_debug_off() {
        let mut ini = Ini::new();
        ini.read(DEFAULT_INI.to_owned()).unwrap();
        let debug = ini.get_map_ref().get("debug").unwrap();
        assert_eq!(debug.len(), 1);
        assert_eq!(ini.get("Debug", "FrameStats").as_deref(), Some("0"));
        for key in [
            "ScatterProbe",
            "ForceStepObjectDistance",
            "ForceStepScatterDistance",
            "ForceStepTerrainDistance",
        ] {
            assert!(!debug.contains_key(&key.to_lowercase()));
        }
        let (c, w) = parse(DEFAULT_INI);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.debug, DebugCfg::default());
        assert_eq!(Config::default().debug, DebugCfg::default());
        assert_eq!(
            DebugCfg::default(),
            DebugCfg {
                frame_stats: false,
                scatter_probe: false,
                force_step_object: None,
                force_step_scatter: None,
                force_step_terrain: None,
            }
        );
    }

    #[test]
    fn every_force_step_key_keeps_0_to_4_without_a_warning() {
        for step in 0u8..=4 {
            let (c, w) = parse(&format!(
                "[Debug]\nForceStepObjectDistance={step}\nForceStepScatterDistance={step}\n\
                 ForceStepTerrainDistance={step}\n"
            ));
            assert!(w.is_empty(), "{step}: {w:?}");
            assert_eq!(c.debug.force_step_object, Some(step));
            assert_eq!(c.debug.force_step_scatter, Some(step));
            assert_eq!(c.debug.force_step_terrain, Some(step));
            assert!(!c.debug.scatter_probe);
        }
    }

    #[test]
    fn a_force_step_below_0_clamps_to_0_and_garbage_is_absent_with_a_warning() {
        let (c, w) = one("Debug", "ForceStepObjectDistance", "-3");
        assert_eq!(c.debug.force_step_object, Some(0));
        assert_eq!(
            w,
            ["[Debug] ForceStepObjectDistance: -3 is outside 0..=4, using 0"]
        );
        for garbage in ["two", "2.5", ""] {
            let (c, w) = one("Debug", "ForceStepTerrainDistance", garbage);
            assert_eq!(c.debug.force_step_terrain, None, "{garbage:?}");
            assert_eq!(w.len(), 1, "{garbage:?}: {w:?}");
            assert!(
                w[0].starts_with("[Debug] ForceStepTerrainDistance: "),
                "{garbage:?}: {w:?}"
            );
        }
    }

    #[test]
    fn scatter_probe_takes_the_usual_booleans_and_debug_is_a_known_section() {
        for (text, want) in [("1", true), ("true", true), ("0", false), ("FALSE", false)] {
            let (c, w) = one("Debug", "ScatterProbe", text);
            assert!(w.is_empty(), "{text}: {w:?}");
            assert_eq!(c.debug.scatter_probe, want, "{text}");
        }
        let (c, w) = one("Debug", "ScatterProbe", "yes");
        assert!(!c.debug.scatter_probe);
        assert_eq!(w.len(), 1, "{w:?}");
        let (_, w) = parse("[debug]\nscatterprobe=1\nNope=1\n");
        assert_eq!(w, ["[Debug] nope: unknown key, ignored"], "{w:?}");
    }

    /// Properties exercise the public parser, independently of its range constants.
    mod properties {
        use super::*;
        use proptest::prelude::*;

        /// Destructure every field so a new setting requires a coverage decision.
        fn assert_ranges(config: &Config) {
            let Config {
                general:
                    General {
                        enabled: _,
                        texture_budget,
                        texture_reserve_gib,
                        async_compute: _,
                        priority: _,
                        reflex: _,
                        anti_lag: _,
                        allow_unknown_build: _,
                        log_level: _,
                    },
                governor:
                    GovernorCfg {
                        target_fps,
                        window_seconds,
                        raise_below,
                        raise_hold_seconds,
                        settle_seconds,
                        lower_above,
                        lower_hold_seconds,
                        hitch_factor,
                        block_seconds,
                        min_vram_free_mib,
                    },
                knobs:
                    KnobsCfg {
                        enable_object_distance: _,
                        max_object_distance,
                        enable_scatter_distance: _,
                        max_scatter_distance,
                        enable_terrain_distance: _,
                        max_terrain_distance,
                        deco_distance_scale,
                        distant_objects_scale,
                        terrain_ring_scale,
                    },
                hdr:
                    HdrCfg {
                        paper_white_nits,
                        fix_peak: _,
                        display_peak_nits,
                    },
                debug:
                    DebugCfg {
                        frame_stats: _,
                        scatter_probe: _,
                        force_step_object,
                        force_step_scatter,
                        force_step_terrain,
                    },
                shadows: _,
            } = *config;
            assert!(target_fps == 0 || (30..=360).contains(&target_fps));
            assert!((4.0..=16.0).contains(&texture_reserve_gib));
            if let TextureBudget::Gib(value) = texture_budget {
                assert!((2.0..=8.0).contains(&value));
            }
            for (value, low, high) in [
                (window_seconds, 0.5, 10.0),
                (raise_below, 0.5, 0.99),
                (raise_hold_seconds, 0.1, 10.0),
                (settle_seconds, 0.1, 10.0),
                (lower_above, 0.8, 1.0),
                (lower_hold_seconds, 0.1, 10.0),
                (hitch_factor, 1.2, 5.0),
                (block_seconds, 1.0, 300.0),
                (deco_distance_scale, 0.5, 1.0),
                (distant_objects_scale, 0.5, 1.0),
                (terrain_ring_scale, 1.0, 1.6),
            ] {
                assert!(
                    (low..=high).contains(&value),
                    "{value} outside {low}..={high}"
                );
            }
            assert!(lower_above > raise_below);
            assert!(min_vram_free_mib <= 8192);
            for step in [
                max_object_distance,
                max_scatter_distance,
                max_terrain_distance,
            ] {
                assert!(step <= 4);
            }
            if let PaperWhiteNits::Nits(value) = paper_white_nits {
                assert!((40.0..=400.0).contains(&value));
            }
            if let Some(value) = display_peak_nits {
                assert!((80..=10000).contains(&value));
            }
            for step in [force_step_object, force_step_scatter, force_step_terrain]
                .into_iter()
                .flatten()
            {
                assert!(step <= 4);
            }
        }

        /// Numeric spelling and fallback policy, taken from the settings documentation.
        #[derive(Clone, Copy, PartialEq)]
        enum Kind {
            Float,
            Whole,
            Target,
            PaperWhite,
            DisplayPeak,
            ForceStep,
            Boolean,
        }

        /// One documented numeric key and its independent, literal bounds.
        struct NumericKey {
            section: &'static str,
            key: &'static str,
            low: f64,
            high: f64,
            kind: Kind,
        }

        /// All bounded numbers, plus the six switches whose accepted numbers are 0 and 1.
        const NUMERIC_KEYS: &[NumericKey] = &[
            NumericKey {
                section: "General",
                key: "TextureBudget",
                low: 2.0,
                high: 8.0,
                kind: Kind::Float,
            },
            NumericKey {
                section: "General",
                key: "TextureReserveGiB",
                low: 4.0,
                high: 16.0,
                kind: Kind::Float,
            },
            NumericKey {
                section: "Governor",
                key: "TargetFps",
                low: 30.0,
                high: 360.0,
                kind: Kind::Target,
            },
            NumericKey {
                section: "Governor",
                key: "WindowSeconds",
                low: 0.5,
                high: 10.0,
                kind: Kind::Float,
            },
            NumericKey {
                section: "Governor",
                key: "RaiseBelow",
                low: 0.5,
                high: 0.99,
                kind: Kind::Float,
            },
            NumericKey {
                section: "Governor",
                key: "RaiseHoldSeconds",
                low: 0.1,
                high: 10.0,
                kind: Kind::Float,
            },
            NumericKey {
                section: "Governor",
                key: "SettleSeconds",
                low: 0.1,
                high: 10.0,
                kind: Kind::Float,
            },
            NumericKey {
                section: "Governor",
                key: "LowerAbove",
                low: 0.8,
                high: 1.0,
                kind: Kind::Float,
            },
            NumericKey {
                section: "Governor",
                key: "LowerHoldSeconds",
                low: 0.1,
                high: 10.0,
                kind: Kind::Float,
            },
            NumericKey {
                section: "Governor",
                key: "HitchFactor",
                low: 1.2,
                high: 5.0,
                kind: Kind::Float,
            },
            NumericKey {
                section: "Governor",
                key: "BlockSeconds",
                low: 1.0,
                high: 300.0,
                kind: Kind::Float,
            },
            NumericKey {
                section: "Governor",
                key: "MinVramFreeMiB",
                low: 0.0,
                high: 8192.0,
                kind: Kind::Whole,
            },
            NumericKey {
                section: "Knobs",
                key: "MaxObjectDistance",
                low: 0.0,
                high: 4.0,
                kind: Kind::Whole,
            },
            NumericKey {
                section: "Knobs",
                key: "MaxScatterDistance",
                low: 0.0,
                high: 4.0,
                kind: Kind::Whole,
            },
            NumericKey {
                section: "Knobs",
                key: "MaxTerrainDistance",
                low: 0.0,
                high: 4.0,
                kind: Kind::Whole,
            },
            NumericKey {
                section: "Knobs",
                key: "DecoDistanceScale",
                low: 0.5,
                high: 1.0,
                kind: Kind::Float,
            },
            NumericKey {
                section: "Knobs",
                key: "DistantObjectsScale",
                low: 0.5,
                high: 1.0,
                kind: Kind::Float,
            },
            NumericKey {
                section: "Knobs",
                key: "TerrainRingScale",
                low: 1.0,
                high: 1.6,
                kind: Kind::Float,
            },
            NumericKey {
                section: "Hdr",
                key: "PaperWhiteNits",
                low: 40.0,
                high: 400.0,
                kind: Kind::PaperWhite,
            },
            NumericKey {
                section: "Hdr",
                key: "DisplayPeakNits",
                low: 80.0,
                high: 10000.0,
                kind: Kind::DisplayPeak,
            },
            NumericKey {
                section: "Debug",
                key: "ForceStepObjectDistance",
                low: 0.0,
                high: 4.0,
                kind: Kind::ForceStep,
            },
            NumericKey {
                section: "Debug",
                key: "ForceStepScatterDistance",
                low: 0.0,
                high: 4.0,
                kind: Kind::ForceStep,
            },
            NumericKey {
                section: "Debug",
                key: "ForceStepTerrainDistance",
                low: 0.0,
                high: 4.0,
                kind: Kind::ForceStep,
            },
            NumericKey {
                section: "General",
                key: "Enabled",
                low: 0.0,
                high: 1.0,
                kind: Kind::Boolean,
            },
            NumericKey {
                section: "General",
                key: "AllowUnknownBuild",
                low: 0.0,
                high: 1.0,
                kind: Kind::Boolean,
            },
            NumericKey {
                section: "Knobs",
                key: "EnableObjectDistance",
                low: 0.0,
                high: 1.0,
                kind: Kind::Boolean,
            },
            NumericKey {
                section: "Knobs",
                key: "EnableScatterDistance",
                low: 0.0,
                high: 1.0,
                kind: Kind::Boolean,
            },
            NumericKey {
                section: "Knobs",
                key: "EnableTerrainDistance",
                low: 0.0,
                high: 1.0,
                kind: Kind::Boolean,
            },
            NumericKey {
                section: "Debug",
                key: "ScatterProbe",
                low: 0.0,
                high: 1.0,
                kind: Kind::Boolean,
            },
        ];

        /// Set one key without causing an unrelated threshold-order warning.
        fn fixture(key: &NumericKey, text: &str) -> String {
            let companion = match key.key {
                "RaiseBelow" => "LowerAbove=1.0\n",
                "LowerAbove" => "RaiseBelow=0.5\n",
                _ => "",
            };
            format!("[{}]\n{}={text}\n{companion}", key.section, key.key)
        }

        /// Read the selected numeric result; word-valued alternatives are represented by None.
        fn numeric_value(config: &Config, key: &str) -> Option<f64> {
            Some(match key {
                "TextureBudget" => match config.general.texture_budget {
                    TextureBudget::Gib(value) => value,
                    TextureBudget::Auto | TextureBudget::Off => return None,
                },
                "PaperWhiteNits" => match config.hdr.paper_white_nits {
                    PaperWhiteNits::Nits(value) => f64::from(value),
                    PaperWhiteNits::Game => return None,
                },
                "DisplayPeakNits" => f64::from(config.hdr.display_peak_nits?),
                "ForceStepObjectDistance" => f64::from(config.debug.force_step_object?),
                "ForceStepScatterDistance" => f64::from(config.debug.force_step_scatter?),
                "ForceStepTerrainDistance" => f64::from(config.debug.force_step_terrain?),
                "Enabled" => f64::from(config.general.enabled),
                "AllowUnknownBuild" => f64::from(config.general.allow_unknown_build),
                "EnableObjectDistance" => f64::from(config.knobs.enable_object_distance),
                "EnableScatterDistance" => f64::from(config.knobs.enable_scatter_distance),
                "EnableTerrainDistance" => f64::from(config.knobs.enable_terrain_distance),
                "ScatterProbe" => f64::from(config.debug.scatter_probe),
                _ => number(config, key),
            })
        }

        /// Validate exactly one selected-key warning as well as the complete returned config.
        fn warned_parse(key: &NumericKey, text: &str) -> Config {
            let (config, warnings) = parse(&fixture(key, text));
            assert_ranges(&config);
            assert_eq!(warnings.len(), 1, "{}={text}: {warnings:?}", key.key);
            assert!(
                warnings[0].starts_with(&format!("[{}] {}:", key.section, key.key)),
                "{warnings:?}"
            );
            config
        }

        proptest! {
            #![proptest_config(ProptestConfig { cases: 32, max_shrink_iters: 128, ..ProptestConfig::default() })]

            #[test]
            fn arbitrary_text_never_panics_and_stays_in_range(
                unicode in any::<String>(),
                bytes in prop::collection::vec(any::<u8>(), 0..256),
                long_len in 16_384usize..65_536,
            ) {
                let mut invalid = vec![0xff, 0xfe];
                invalid.extend(bytes);
                let lossy = String::from_utf8_lossy(&invalid).into_owned();
                let line = "9".repeat(long_len);
                for text in [
                    unicode.clone(), lossy, format!("\u{fffd}\0\r{unicode}"), line.clone(),
                    format!("[General]\r\nTextureBudget={line}\r"),
                    format!("[Hdr]\nPaperWhiteNits={unicode}\0\r"),
                ] {
                    let (config, _) = parse(&text);
                    assert_ranges(&config);
                }
            }

            #[test]
            fn outside_values_warn_once_for_every_numeric_key(delta in 1u32..10_000) {
                for key in NUMERIC_KEYS {
                    let below = if key.kind == Kind::Target {
                        // Zero is valid; exercise the invalid positive gap using integers.
                        f64::from(1 + delta % 29)
                    } else { key.low - f64::from(delta) };
                    for value in [below, key.high + f64::from(delta)] {
                        let config = warned_parse(key, &value.to_string());
                        let expected = match key.kind {
                            Kind::PaperWhite | Kind::DisplayPeak => None,
                            Kind::Boolean => numeric_value(&Config::default(), key.key),
                            _ => Some(value.clamp(key.low, key.high)),
                        };
                        assert_eq!(numeric_value(&config, key.key), expected, "{}={value}", key.key);
                    }
                }
            }

            #[test]
            fn decimal_comma_matches_dot_for_every_numeric_key(sample in any::<u32>()) {
                let (zero, warnings) = parse("[Governor]\nTargetFps=0");
                assert_eq!(zero.governor.target_fps, 0);
                assert!(warnings.is_empty());
                assert_ranges(&zero);
                for key in NUMERIC_KEYS.iter().filter(|key| key.kind != Kind::Boolean) {
                    let value = match key.kind {
                        Kind::Whole | Kind::Target | Kind::ForceStep | Kind::DisplayPeak => key.low + f64::from(sample) % (key.high - key.low + 1.0),
                        _ => key.low + (key.high - key.low) * f64::from(sample % 1_001) / 1_000.0,
                    };
                    let dot = format!("{value:.6}");
                    let comma = dot.replace('.', ",");
                    let (dot_config, dot_warnings) = parse(&fixture(key, &dot));
                    let (comma_config, comma_warnings) = parse(&fixture(key, &comma));
                    assert!(dot_warnings.is_empty(), "{}={dot}: {dot_warnings:?}", key.key);
                    assert!(comma_warnings.is_empty(), "{}={comma}: {comma_warnings:?}", key.key);
                    assert_eq!(dot_config, comma_config, "{}={dot}/{comma}", key.key);
                    assert_ranges(&dot_config);
                    assert_ranges(&comma_config);
                }
            }

            #[test]
            fn nonfinite_numbers_use_defaults_for_every_numeric_key(which in 0usize..4) {
                let text = ["NaN", "inf", "-inf", "1e999"][which];
                for key in NUMERIC_KEYS {
                    let config = warned_parse(key, text);
                    assert_eq!(numeric_value(&config, key.key), numeric_value(&Config::default(), key.key), "{}={text}", key.key);
                }
            }

            #[test]
            fn fractional_integers_warn_and_use_the_default(sample in 0u32..1_000) {
                for key in NUMERIC_KEYS.iter().filter(|key| matches!(key.kind, Kind::Whole | Kind::Target | Kind::ForceStep | Kind::DisplayPeak)) {
                    let value = key.low + f64::from(sample) % (key.high - key.low) + 0.5;
                    let config = warned_parse(key, &value.to_string());
                    assert_eq!(numeric_value(&config, key.key), numeric_value(&Config::default(), key.key), "{}={value}", key.key);
                }
            }

            #[test]
            fn unordered_thresholds_reset_both_and_warn(raise in 80u32..=99, lower in 80u32..=99) {
                let raise = f64::from(raise) / 100.0;
                let lower = (f64::from(lower) / 100.0).min(raise);
                let (config, warnings) = parse(&format!("[Governor]\nRaiseBelow={raise}\nLowerAbove={lower}"));
                assert_eq!(config.governor.raise_below, 0.90);
                assert_eq!(config.governor.lower_above, 0.97);
                assert_eq!(warnings.len(), 1, "{warnings:?}");
                assert!(warnings[0].contains("RaiseBelow") && warnings[0].contains("LowerAbove"));
                assert_ranges(&config);
            }
        }
    }
}

/// Settings metadata, available without loading an INI file.
pub fn schema() -> &'static [emberkit::config::Setting] {
    use emberkit::config::{Setting, SettingType};
    &[
        Setting {
            section: "Shadows",
            key: "PointLightRays",
            kind: SettingType::Int {
                min: 65,
                max: 256,
                zero: false,
                words: &["game"],
            },
            default: Some("game"),
            description: "Experimental startup shadow override; game preserves vanilla.",
            needs_restart: true,
        },
        Setting {
            section: "Shadows",
            key: "PointLightStaticDistance",
            kind: SettingType::Float {
                min: 101.0,
                max: 400.0,
                words: &["game"],
            },
            default: Some("game"),
            description: "Experimental startup shadow override; game preserves vanilla.",
            needs_restart: true,
        },
        Setting {
            section: "Shadows",
            key: "PointLightTile",
            kind: SettingType::Choice(&["game", "224", "256"]),
            default: Some("game"),
            description: "Experimental startup shadow override; game preserves vanilla.",
            needs_restart: true,
        },
        Setting {
            section: "Shadows",
            key: "SunResolution",
            kind: SettingType::Choice(&["game", "4096"]),
            default: Some("game"),
            description: "Experimental startup shadow override; game preserves vanilla.",
            needs_restart: true,
        },
        Setting {
            section: "General",
            key: "Enabled",
            kind: SettingType::Bool,
            default: Some("1"),
            description: "Enable this mod at startup.",
            needs_restart: true,
        },
        Setting {
            section: "General",
            key: "TextureBudget",
            kind: SettingType::Float {
                min: 2.0,
                max: 8.0,
                words: &["auto", "off"],
            },
            default: Some("auto"),
            description: "Select the texture-streaming budget.",
            needs_restart: true,
        },
        Setting {
            section: "General",
            key: "TextureReserveGiB",
            kind: SettingType::Float {
                min: 4.0,
                max: 16.0,
                words: &[],
            },
            default: Some("11"),
            description: "Reserve VRAM for other rendering.",
            needs_restart: true,
        },
        Setting {
            section: "General",
            key: "AsyncCompute",
            kind: SettingType::Choice(&["on", "game"]),
            default: Some("on"),
            description: "Enable async compute or follow the game.",
            needs_restart: true,
        },
        Setting {
            section: "General",
            key: "Priority",
            kind: SettingType::Choice(&["above_normal", "normal"]),
            default: Some("above_normal"),
            description: "Set priority after renderer creation.",
            needs_restart: true,
        },
        Setting {
            section: "General",
            key: "Reflex",
            kind: SettingType::Choice(&["game", "auto", "off", "on", "boost"]),
            default: Some("auto"),
            description: "Select supported NVIDIA latency mode.",
            needs_restart: true,
        },
        Setting {
            section: "General",
            key: "AntiLag",
            kind: SettingType::Choice(&["auto", "off", "on"]),
            default: Some("auto"),
            description: "Select experimental AMD Anti-Lag.",
            needs_restart: true,
        },
        Setting {
            section: "General",
            key: "AllowUnknownBuild",
            kind: SettingType::Bool,
            default: Some("0"),
            description: "Allow checked code patches on an unknown build.",
            needs_restart: true,
        },
        Setting {
            section: "General",
            key: "LogLevel",
            kind: SettingType::Choice(&["error", "warn", "info", "debug"]),
            default: Some("info"),
            description: "Select the highest written log level.",
            needs_restart: true,
        },
        Setting {
            section: "Governor",
            key: "TargetFps",
            kind: SettingType::Int {
                min: 30,
                max: 360,
                zero: true,
                words: &[],
            },
            default: Some("75"),
            description: "Set target FPS;0 disables the governor.",
            needs_restart: true,
        },
        Setting {
            section: "Governor",
            key: "WindowSeconds",
            kind: SettingType::Float {
                min: 0.5,
                max: 10.0,
                words: &[],
            },
            default: Some("2"),
            description: "Set the measured frame window.",
            needs_restart: true,
        },
        Setting {
            section: "Governor",
            key: "RaiseBelow",
            kind: SettingType::Float {
                min: 0.5,
                max: 0.99,
                words: &[],
            },
            default: Some("0.90"),
            description: "Set p95 headroom threshold.",
            needs_restart: true,
        },
        Setting {
            section: "Governor",
            key: "RaiseHoldSeconds",
            kind: SettingType::Float {
                min: 0.1,
                max: 10.0,
                words: &[],
            },
            default: Some("1.0"),
            description: "Hold headroom before raising.",
            needs_restart: true,
        },
        Setting {
            section: "Governor",
            key: "SettleSeconds",
            kind: SettingType::Float {
                min: 0.1,
                max: 10.0,
                words: &[],
            },
            default: Some("1.0"),
            description: "Wait after a distance change.",
            needs_restart: true,
        },
        Setting {
            section: "Governor",
            key: "LowerAbove",
            kind: SettingType::Float {
                min: 0.8,
                max: 1.0,
                words: &[],
            },
            default: Some("0.97"),
            description: "Set overload threshold.",
            needs_restart: true,
        },
        Setting {
            section: "Governor",
            key: "LowerHoldSeconds",
            kind: SettingType::Float {
                min: 0.1,
                max: 10.0,
                words: &[],
            },
            default: Some("0.3"),
            description: "Hold overload before undoing.",
            needs_restart: true,
        },
        Setting {
            section: "Governor",
            key: "HitchFactor",
            kind: SettingType::Float {
                min: 1.2,
                max: 5.0,
                words: &[],
            },
            default: Some("2.0"),
            description: "Set the post-raise hitch multiplier.",
            needs_restart: true,
        },
        Setting {
            section: "Governor",
            key: "BlockSeconds",
            kind: SettingType::Float {
                min: 1.0,
                max: 300.0,
                words: &[],
            },
            default: Some("20"),
            description: "Set base undo block and probation duration.",
            needs_restart: true,
        },
        Setting {
            section: "Governor",
            key: "MinVramFreeMiB",
            kind: SettingType::Int {
                min: 0,
                max: 8192,
                zero: false,
                words: &[],
            },
            default: Some("1024"),
            description: "Block costly raises below this free budget.",
            needs_restart: true,
        },
        Setting {
            section: "Knobs",
            key: "EnableObjectDistance",
            kind: SettingType::Bool,
            default: Some("1"),
            description: "Enable the object-distance knob.",
            needs_restart: true,
        },
        Setting {
            section: "Knobs",
            key: "MaxObjectDistance",
            kind: SettingType::Int {
                min: 0,
                max: 4,
                zero: false,
                words: &[],
            },
            default: Some("4"),
            description: "Cap the object's added step.",
            needs_restart: true,
        },
        Setting {
            section: "Knobs",
            key: "EnableScatterDistance",
            kind: SettingType::Bool,
            default: Some("1"),
            description: "Enable the grass-distance knob.",
            needs_restart: true,
        },
        Setting {
            section: "Knobs",
            key: "MaxScatterDistance",
            kind: SettingType::Int {
                min: 0,
                max: 4,
                zero: false,
                words: &[],
            },
            default: Some("4"),
            description: "Cap the grass added step.",
            needs_restart: true,
        },
        Setting {
            section: "Knobs",
            key: "EnableTerrainDistance",
            kind: SettingType::Bool,
            default: Some("1"),
            description: "Enable the terrain-distance knob.",
            needs_restart: true,
        },
        Setting {
            section: "Knobs",
            key: "MaxTerrainDistance",
            kind: SettingType::Int {
                min: 0,
                max: 4,
                zero: false,
                words: &[],
            },
            default: Some("4"),
            description: "Cap the terrain added step.",
            needs_restart: true,
        },
        Setting {
            section: "Knobs",
            key: "DecoDistanceScale",
            kind: SettingType::Float {
                min: 0.5,
                max: 1.0,
                words: &[],
            },
            default: Some("1.0"),
            description: "Scale the decoration startup constant.",
            needs_restart: true,
        },
        Setting {
            section: "Knobs",
            key: "DistantObjectsScale",
            kind: SettingType::Float {
                min: 0.5,
                max: 1.0,
                words: &[],
            },
            default: Some("1.0"),
            description: "Scale the distant-object startup constant.",
            needs_restart: true,
        },
        Setting {
            section: "Knobs",
            key: "TerrainRingScale",
            kind: SettingType::Float {
                min: 1.0,
                max: 1.6,
                words: &[],
            },
            default: Some("1.0"),
            description: "Scale terrain startup rings.",
            needs_restart: true,
        },
        Setting {
            section: "Hdr",
            key: "PaperWhiteNits",
            kind: SettingType::Float {
                min: 40.0,
                max: 400.0,
                words: &["game"],
            },
            default: Some("game"),
            description: "Set HDR paper white.",
            needs_restart: true,
        },
        Setting {
            section: "Hdr",
            key: "FixPeak",
            kind: SettingType::Choice(&["on", "off"]),
            default: Some("off"),
            description: "Correct the HDR tonemapper peak.",
            needs_restart: true,
        },
        Setting {
            section: "Hdr",
            key: "DisplayPeakNits",
            kind: SettingType::Int {
                min: 80,
                max: 10000,
                zero: false,
                words: &["game"],
            },
            default: Some("game"),
            description: "Override panel peak when FixPeak is on.",
            needs_restart: true,
        },
        Setting {
            section: "Debug",
            key: "FrameStats",
            kind: SettingType::Bool,
            default: Some("0"),
            description: "Permit debug frame reports when TargetFps=0; requires LogLevel=debug.",
            needs_restart: true,
        },
        Setting {
            section: "Debug",
            key: "ScatterProbe",
            kind: SettingType::Bool,
            default: Some("0"),
            description: "Request scatter diagnostics at debug log level.",
            needs_restart: true,
        },
        Setting {
            section: "Debug",
            key: "ForceStepObjectDistance",
            kind: SettingType::Int {
                min: 0,
                max: 4,
                zero: false,
                words: &[],
            },
            default: None,
            description: "Hold an object step at debug log level.",
            needs_restart: true,
        },
        Setting {
            section: "Debug",
            key: "ForceStepScatterDistance",
            kind: SettingType::Int {
                min: 0,
                max: 4,
                zero: false,
                words: &[],
            },
            default: None,
            description: "Hold a grass step at debug log level.",
            needs_restart: true,
        },
        Setting {
            section: "Debug",
            key: "ForceStepTerrainDistance",
            kind: SettingType::Int {
                min: 0,
                max: 4,
                zero: false,
                words: &[],
            },
            default: None,
            description: "Hold a terrain step at debug log level.",
            needs_restart: true,
        },
    ]
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    use std::collections::BTreeSet;
    #[test]
    fn schema_matches_defaults_and_exact_debug_omissions() {
        let schema = schema();
        emberkit::config::take_requested_keys();
        parse(DEFAULT_INI);
        let requested = emberkit::config::take_requested_keys();
        let described: BTreeSet<_> = schema
            .iter()
            .map(|s| (s.section.to_lowercase(), s.key.to_lowercase()))
            .collect();
        assert_eq!(requested, described);
        assert_eq!(schema.len(), 40);
        let mut ini = Ini::new();
        ini.read(DEFAULT_INI.into()).unwrap();
        let actual: BTreeSet<_> = schema
            .iter()
            .map(|s| (s.section.to_lowercase(), s.key.to_lowercase()))
            .collect();
        assert_eq!(actual.len(), 40);
        let defaults: BTreeSet<_> = ini
            .get_map_ref()
            .iter()
            .flat_map(|(section, keys)| keys.keys().map(move |key| (section.clone(), key.clone())))
            .collect();
        let omitted: BTreeSet<_> = [
            "ScatterProbe",
            "ForceStepObjectDistance",
            "ForceStepScatterDistance",
            "ForceStepTerrainDistance",
        ]
        .into_iter()
        .map(|key| ("debug".to_owned(), key.to_lowercase()))
        .collect();
        assert_eq!(
            actual
                .difference(&defaults)
                .cloned()
                .collect::<BTreeSet<_>>(),
            omitted
        );
        assert!(defaults.is_subset(&actual));
        for entry in schema {
            assert!(entry.needs_restart);
            assert!(!entry.description.is_empty());
            if defaults.contains(&(entry.section.to_lowercase(), entry.key.to_lowercase())) {
                assert_eq!(ini.get(entry.section, entry.key).as_deref(), entry.default);
            }
        }
        for entry in schema.iter().filter(|s| s.key.starts_with("ForceStep")) {
            assert_eq!(entry.default, None);
        }
    }
}

#[cfg(test)]
mod frame_stats_setting_tests {
    use super::*;
    #[test]
    fn frame_stats_defaults_off_accepts_boolean_and_warns_on_invalid() {
        assert!(!Config::default().debug.frame_stats);
        let (cfg, warnings) = parse("[Debug]\nFrameStats=1\n");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(cfg.debug.frame_stats);
        let (cfg, warnings) = parse("[Debug]\nFrameStats=garbage\n");
        assert!(!cfg.debug.frame_stats);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("default 0"));
    }
}
