//! The settings file, `enshrouded-headroom.ini`.
//!
//! `parse` never fails and never panics. A value that is missing, not valid or out of range falls
//! back to its default or is clamped, and each such case adds one line to the returned warnings.
//! The caller logs the warnings. Section and key names are not case sensitive.

use std::collections::HashSet;
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

/// The `[Debug]` section (design.md section 8). It is not in the default file, so every key is off
/// unless the player adds it. It is for testing only: a consumer honours it only with
/// `LogLevel = debug`, and that check is the consumer's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DebugCfg {
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

/// All settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    pub general: General,
    pub governor: GovernorCfg,
    pub knobs: KnobsCfg,
    pub hdr: HdrCfg,
    /// The `[Debug]` keys; all off by default.
    pub debug: DebugCfg,
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
    debug: DebugCfg {
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
const SECTIONS: [&str; 5] = ["General", "Governor", "Knobs", "Hdr", "Debug"];

/// Parse the text of a settings file. The `Vec` holds one warning per problem found.
pub fn parse(text: &str) -> (Config, Vec<String>) {
    // Notepad can save UTF-8 with a byte order mark. It would hide the first `[`.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut ini = Ini::new();
    if let Err(error) = ini.read(text.to_owned()) {
        let warning = format!("the settings file cannot be read ({error}), using all defaults");
        return (BUILTIN, vec![warning]);
    }
    let mut r = Reader {
        ini,
        used: HashSet::new(),
        warnings: Vec::new(),
    };
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
        r.warnings.push(format!(
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
        scatter_probe: r.boolean("Debug", "ScatterProbe", d.debug.scatter_probe),
        force_step_object: r.force_step("ForceStepObjectDistance"),
        force_step_scatter: r.force_step("ForceStepScatterDistance"),
        force_step_terrain: r.force_step("ForceStepTerrainDistance"),
    };

    r.report_unknown();
    (
        Config {
            general,
            governor,
            knobs,
            hdr,
            debug,
        },
        r.warnings,
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

/// The most characters of the player's own text that one warning repeats.
const MAX_ECHO_CHARS: usize = 64;

/// `text`, cut to `MAX_ECHO_CHARS` characters (on a character boundary) with `…` added if it was
/// cut. A value or key can be as long as the file, and a log line must not be.
fn shorten(text: &str) -> String {
    match text.char_indices().nth(MAX_ECHO_CHARS) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// A number as a person types it. A decimal comma (`0,9`) counts. `nan` and `inf` do not.
fn parse_number(text: &str) -> Option<f64> {
    let value: f64 = text.trim().replace(',', ".").parse().ok()?;
    value.is_finite().then_some(value)
}

/// The parsed file plus the warnings so far. The typed getters mark each key they ask for, so the
/// keys left over at the end are the unknown ones.
struct Reader {
    ini: Ini,
    /// Lower-case `(section, key)` pairs that a getter has asked for.
    used: HashSet<(String, String)>,
    warnings: Vec<String>,
}

impl Reader {
    /// The text of `[section] key`, or `None` if the file does not have it. A key written without
    /// `=` has no text, which reads as an empty value.
    fn raw(&mut self, section: &str, key: &str) -> Option<String> {
        // configparser stores section and key names in lower case.
        let (section, key) = (section.to_lowercase(), key.to_lowercase());
        let text = self
            .ini
            .get_map_ref()
            .get(&section)?
            .get(&key)?
            .clone()
            .unwrap_or_default();
        self.used.insert((section, key));
        Some(text)
    }

    /// Warn that `text` is not a valid value for `[section] key`.
    fn not_valid(&mut self, section: &str, key: &str, text: &str, expected: &str, default: &str) {
        let text = shorten(text);
        self.warnings.push(format!(
            "[{section}] {key}: \"{text}\" is not {expected}, using the default {default}"
        ));
    }

    /// Clamp into `lo..=hi`, with a warning if that changed the value.
    fn clamp(&mut self, section: &str, key: &str, value: f64, lo: f64, hi: f64) -> f64 {
        let kept = value.clamp(lo, hi);
        if !(lo..=hi).contains(&value) {
            // `1e300` prints as 301 digits.
            let value = shorten(&value.to_string());
            self.warnings.push(format!(
                "[{section}] {key}: {value} is outside {lo}..={hi}, using {kept}"
            ));
        }
        kept
    }

    fn float(&mut self, section: &str, key: &str, default: f64, lo: f64, hi: f64) -> f64 {
        let Some(text) = self.raw(section, key) else {
            return default;
        };
        match parse_number(&text) {
            Some(value) => self.clamp(section, key, value, lo, hi),
            None => {
                self.not_valid(section, key, &text, "a number", &default.to_string());
                default
            }
        }
    }

    /// A whole number in `lo..=hi`. With `zero_ok`, `0` is allowed as well. A number with a zero
    /// fraction counts as whole (`60.0`, `4,0`); `60.5` does not.
    fn int(
        &mut self,
        section: &str,
        key: &str,
        default: u32,
        lo: u32,
        hi: u32,
        zero_ok: bool,
    ) -> u32 {
        let Some(text) = self.raw(section, key) else {
            return default;
        };
        let Some(number) = parse_number(&text).filter(|number| number.fract() == 0.0) else {
            self.not_valid(section, key, &text, "a whole number", &default.to_string());
            return default;
        };
        // A float-to-integer `as` cast saturates, so a huge number becomes `i64::MAX` or `i64::MIN`
        // and is then clamped like any other number that is too big.
        let value = number as i64;
        let kept = if zero_ok && value == 0 {
            0
        } else {
            value.clamp(i64::from(lo), i64::from(hi))
        };
        if kept != value {
            let range = if zero_ok {
                format!("0 or {lo}..={hi}")
            } else {
                format!("{lo}..={hi}")
            };
            self.warnings.push(format!(
                "[{section}] {key}: {value} is outside {range}, using {kept}"
            ));
        }
        // `kept` is between 0 and `hi`, so this cannot fail.
        u32::try_from(kept).unwrap_or(default)
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
            self.warnings.push(format!(
                "[Debug] {key}: {value} is outside 0..=4, using {kept}"
            ));
        }
        u8::try_from(kept).ok()
    }

    fn boolean(&mut self, section: &str, key: &str, default: bool) -> bool {
        let Some(text) = self.raw(section, key) else {
            return default;
        };
        match text.to_ascii_lowercase().as_str() {
            "1" | "true" => true,
            "0" | "false" => false,
            _ => {
                let default_text = if default { "1" } else { "0" };
                self.not_valid(section, key, &text, "0, 1, true or false", default_text);
                default
            }
        }
    }

    /// One of the words in `options`, not case sensitive.
    fn choice<T: Copy + PartialEq>(
        &mut self,
        section: &str,
        key: &str,
        default: T,
        options: &[(&str, T)],
    ) -> T {
        let Some(text) = self.raw(section, key) else {
            return default;
        };
        let word = text.to_ascii_lowercase();
        if let Some(&(_, value)) = options.iter().find(|(name, _)| *name == word) {
            return value;
        }
        let names: Vec<&str> = options.iter().map(|(name, _)| *name).collect();
        let default_name = options
            .iter()
            .find(|(_, value)| *value == default)
            .map_or("?", |(name, _)| *name);
        let expected = format!("one of {}", names.join(", "));
        self.not_valid(section, key, &text, &expected, default_name);
        default
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

    /// Warn about each section and key that no getter asked for.
    fn report_unknown(&mut self) {
        let mut unknown = Vec::new();
        for (section, keys) in self.ini.get_map_ref() {
            let Some(known) = SECTIONS.iter().find(|s| s.to_lowercase() == *section) else {
                unknown.push(if section == "default" {
                    // configparser files keys that come before any header under this name.
                    "[default]: keys before the first section header, or a section called \
                     default, are ignored"
                        .to_owned()
                } else {
                    format!("[{}]: unknown section, ignored", shorten(section))
                });
                continue;
            };
            for key in keys.keys() {
                if !self.used.contains(&(section.clone(), key.clone())) {
                    unknown.push(format!("[{known}] {}: unknown key, ignored", shorten(key)));
                }
            }
        }
        // The map's order is not the file's order; sort so the log is the same every time.
        unknown.sort();
        self.warnings.extend(unknown);
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
            31
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
            Reader {
                ini,
                used: HashSet::new(),
                warnings: Vec::new(),
            }
        };
        for (default, named) in [
            (TextureBudget::Auto, "default auto"),
            (TextureBudget::Off, "default off"),
            (TextureBudget::Gib(6.5), "default 6.5"),
        ] {
            let mut r = reader("lots");
            assert_eq!(r.texture_budget(default), default);
            assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
            assert!(r.warnings[0].contains(named), "{}", r.warnings[0]);
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
    fn the_default_file_has_no_debug_section_and_gives_debug_off() {
        assert!(!DEFAULT_INI.to_ascii_lowercase().contains("[debug]"));
        let (c, w) = parse(DEFAULT_INI);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.debug, DebugCfg::default());
        assert_eq!(Config::default().debug, DebugCfg::default());
        assert_eq!(
            DebugCfg::default(),
            DebugCfg {
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
}
