//! The settings file, `ember-climb.ini`, next to the `.asi`.
//!
//! A bad value warns and falls back to its default. The log lists every warning.

use std::io;
use std::path::Path;

use configparser::ini::Ini;
use emberkit::config::{Reader, Setting, SettingType};

use crate::grace::TICKS_PER_SECOND;
use crate::surface::{Tuning, MAX_OVERHANG_DEGREES};

/// The settings file's name.
pub const INI_FILE_NAME: &str = "ember-climb.ini";
/// The default settings file. A test checks that it says the same as [`Settings::default`].
pub const DEFAULT_INI: &str = include_str!("../ember-climb.default.ini");
/// The collider bits a surface mask may use: WorldNormalsNotDisplaced (2), Solid (3), Walkable (5)
/// and ClimbingWall (8). Bits 0 and 1 are the other two voxel bodies; asking for them as well gives
/// the nearest of two different surfaces, which is the instability this mod exists to avoid.
pub const MASK_BITS: u32 = 0x12c;
/// The section names, for the unknown-key warnings.
const SECTIONS: [&str; 4] = ["Climb", "Start", "Tuning", "Debug"];

/// Every setting, as read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Does the mod do anything?
    pub enabled: bool,
    /// The collider flags that can be climbed.
    pub surface_mask: u32,
    /// How uneven a patch may be to start a climb, in metres.
    pub plane_tolerance: f32,
    /// Climbing stamina cost, as a multiple of the game's.
    pub stamina_multiplier: f32,
    /// Does walking into a surface start a climb?
    pub walk: bool,
    /// Does jumping or falling into one?
    pub jump: bool,
    /// Does sprinting into one?
    pub sprint: bool,
    /// Does gliding into one?
    pub glide: bool,
    /// Surfaces less steep than this are walkable, in degrees.
    pub walkable_angle: f32,
    /// The farthest a face may hang over to start a climb, in degrees.
    pub overhang_angle: f32,
    /// How close the player must be to start a climb, in metres.
    pub entry_reach: f32,
    /// How far the rays reach during a climb, in metres.
    pub hold_reach: f32,
    /// How long a lost surface is forgiven, in seconds.
    pub grace_seconds: f32,
    /// Does the log get the per-second line?
    pub diagnostics: bool,
}

/// The defaults. `DEFAULT_INI` repeats them.
const BUILTIN: Settings = Settings {
    enabled: true,
    surface_mask: 0x124,
    plane_tolerance: 0.45,
    stamina_multiplier: 1.0,
    walk: true,
    jump: true,
    sprint: true,
    glide: false,
    walkable_angle: 50.0,
    overhang_angle: 110.0,
    entry_reach: 0.95,
    hold_reach: 1.0,
    grace_seconds: 0.0,
    diagnostics: true,
};

impl Default for Settings {
    fn default() -> Settings {
        BUILTIN
    }
}

impl Settings {
    /// The surface limits these settings give.
    pub fn tuning(&self) -> Tuning {
        Tuning::new(
            self.surface_mask,
            self.plane_tolerance,
            self.walkable_angle,
            self.overhang_angle,
            (self.entry_reach, self.hold_reach),
        )
    }

    /// The grace time in whole game ticks.
    pub fn grace_ticks(&self) -> u8 {
        // The reader keeps the value in 0..=1, which fits, and a float-to-integer `as` cast cannot
        // panic.
        (self.grace_seconds * TICKS_PER_SECOND).round() as u8
    }
}

/// The mask written as `text`: hex with `0x`, or decimal.
fn mask_of(text: &str) -> Option<u32> {
    let text = text.trim();
    let mask = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => text.parse().ok()?,
    };
    (mask != 0 && mask & !MASK_BITS == 0).then_some(mask)
}

/// Read `[Climb] SurfaceMask`.
fn read_mask(reader: &mut Reader) -> u32 {
    let default = BUILTIN.surface_mask;
    let Some(text) = reader.raw("Climb", "SurfaceMask") else {
        return default;
    };
    mask_of(&text).unwrap_or_else(|| {
        reader.not_valid(
            "Climb",
            "SurfaceMask",
            &text,
            "a non-zero mask of the bits 0x4, 0x8, 0x20 and 0x100",
            &format!("{default:#x}"),
        );
        default
    })
}

/// A float setting, kept in `lo..=hi`.
fn float(reader: &mut Reader, at: (&str, &str), default: f32, lo: f64, hi: f64) -> f32 {
    let (section, key) = at;
    reader.float(section, key, f64::from(default), lo, hi) as f32
}

/// Parse the text of a settings file. The `Vec` holds one warning per problem found.
pub fn parse(text: &str) -> (Settings, Vec<String>) {
    // Notepad can save UTF-8 with a byte order mark. It would hide the first `[`.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut ini = Ini::new();
    if let Err(error) = ini.read(text.to_owned()) {
        let warning = format!("the settings file cannot be read ({error}), using all defaults");
        return (BUILTIN, vec![warning]);
    }
    let mut r = Reader::new(ini);
    let d = BUILTIN;
    // Struct fields are evaluated in order, so warnings come out in file order.
    let settings = Settings {
        enabled: r.boolean("Climb", "Enabled", d.enabled),
        surface_mask: read_mask(&mut r),
        plane_tolerance: float(
            &mut r,
            ("Climb", "PlaneTolerance"),
            d.plane_tolerance,
            0.05,
            1.0,
        ),
        stamina_multiplier: float(
            &mut r,
            ("Climb", "StaminaMultiplier"),
            d.stamina_multiplier,
            0.0,
            10.0,
        ),
        walk: r.boolean("Start", "Walk", d.walk),
        jump: r.boolean("Start", "Jump", d.jump),
        sprint: r.boolean("Start", "Sprint", d.sprint),
        glide: r.boolean("Start", "Glide", d.glide),
        walkable_angle: float(
            &mut r,
            ("Tuning", "WalkableAngle"),
            d.walkable_angle,
            30.0,
            75.0,
        ),
        overhang_angle: float(
            &mut r,
            ("Tuning", "OverhangAngle"),
            d.overhang_angle,
            90.0,
            f64::from(MAX_OVERHANG_DEGREES),
        ),
        entry_reach: float(&mut r, ("Tuning", "EntryReach"), d.entry_reach, 0.6, 1.4),
        hold_reach: float(&mut r, ("Tuning", "HoldReach"), d.hold_reach, 0.8, 1.6),
        grace_seconds: float(
            &mut r,
            ("Tuning", "GraceSeconds"),
            d.grace_seconds,
            0.0,
            1.0,
        ),
        diagnostics: r.boolean("Debug", "Diagnostics", d.diagnostics),
    };
    r.report_unknown(&SECTIONS);
    (settings, r.into_warnings())
}

/// Read the settings from `dir`. `Ok(None)` means there is no file.
pub fn read_ini(dir: &Path) -> io::Result<Option<(Settings, Vec<String>)>> {
    match std::fs::read(dir.join(INI_FILE_NAME)) {
        // Bytes that are not UTF-8 become U+FFFD instead of an error. One odd character in a
        // comment must not cost the player their settings.
        Ok(bytes) => Ok(Some(parse(&String::from_utf8_lossy(&bytes)))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Write [`DEFAULT_INI`] as the settings file in `dir`. An existing file is never replaced.
pub fn write_default(dir: &Path) -> io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(INI_FILE_NAME))?;
    file.write_all(DEFAULT_INI.as_bytes())
}

/// A whole-file bool setting, for [`schema`].
const fn flag(
    section: &'static str,
    key: &'static str,
    default: &'static str,
    description: &'static str,
) -> Setting {
    Setting {
        section,
        key,
        kind: SettingType::Bool,
        default: Some(default),
        description,
        needs_restart: true,
    }
}

/// A bounded float setting, for [`schema`].
const fn number(
    section: &'static str,
    key: &'static str,
    range: (f64, f64),
    default: &'static str,
    description: &'static str,
) -> Setting {
    Setting {
        section,
        key,
        kind: SettingType::Float {
            min: range.0,
            max: range.1,
            words: &[],
        },
        default: Some(default),
        description,
        needs_restart: true,
    }
}

/// Settings metadata, available without loading a file.
pub fn schema() -> &'static [Setting] {
    const SCHEMA: [Setting; 14] = [
        flag("Climb", "Enabled", "1", "Climb any steep surface."),
        Setting {
            section: "Climb",
            key: "SurfaceMask",
            kind: SettingType::Text,
            default: Some("0x124"),
            description: "Colliders that can be climbed: a mask of 0x4, 0x8, 0x20 and 0x100.",
            needs_restart: true,
        },
        number(
            "Climb",
            "PlaneTolerance",
            (0.05, 1.0),
            "0.45",
            "How uneven a patch of surface may be to start a climb, in metres.",
        ),
        number(
            "Climb",
            "StaminaMultiplier",
            (0.0, 10.0),
            "1",
            "Climbing stamina cost, as a multiple of the game's cost.",
        ),
        flag(
            "Start",
            "Walk",
            "1",
            "Walking into a surface starts a climb.",
        ),
        flag(
            "Start",
            "Jump",
            "1",
            "Jumping or falling into one starts a climb.",
        ),
        flag("Start", "Sprint", "1", "Sprinting into one starts a climb."),
        flag(
            "Start",
            "Glide",
            "0",
            "Gliding into one starts a climb (experimental).",
        ),
        number(
            "Tuning",
            "WalkableAngle",
            (30.0, 75.0),
            "50",
            "A surface less steep than this can be walked on, in degrees.",
        ),
        number(
            "Tuning",
            "OverhangAngle",
            (90.0, MAX_OVERHANG_DEGREES as f64),
            "110",
            "The farthest a face may hang over to start a climb, in degrees. Above 135 is experimental; native acceptance is pending.",
        ),
        number(
            "Tuning",
            "EntryReach",
            (0.6, 1.4),
            "0.95",
            "How close the player must be to start a climb, in metres.",
        ),
        number(
            "Tuning",
            "HoldReach",
            (0.8, 1.6),
            "1",
            "How far the player looks for the surface during a climb, in metres.",
        ),
        number(
            "Tuning",
            "GraceSeconds",
            (0.0, 1.0),
            "0",
            "Reserved grace model in seconds; unavailable in native runtime until state initialization is proven.",
        ),
        flag(
            "Debug",
            "Diagnostics",
            "1",
            "Log what each step of a climb did, to show which step failed.",
        ),
    ];
    &SCHEMA
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped file says exactly what the code assumes, with no warnings.
    #[test]
    fn the_default_ini_is_the_defaults() {
        assert_eq!(parse(DEFAULT_INI), (Settings::default(), Vec::new()));
    }

    /// The first line follows the project's rule for files that are not Rust.
    #[test]
    fn the_default_ini_says_why_it_is_not_rust() {
        assert!(DEFAULT_INI.starts_with("; WHY NOT RUST:"));
    }

    /// The first test build must show which step fails, and the risky trigger stays off.
    #[test]
    fn diagnostics_are_on_and_glide_is_off() {
        let settings = Settings::default();
        assert!(settings.diagnostics);
        assert!(!settings.glide);
    }

    /// An empty file is valid and gives the defaults.
    #[test]
    fn an_empty_file_gives_the_defaults() {
        assert_eq!(parse(""), (Settings::default(), Vec::new()));
    }

    /// Notepad's byte order mark does not hide the first section.
    #[test]
    fn a_byte_order_mark_is_ignored() {
        let (settings, warnings) = parse("\u{feff}[Climb]\nEnabled=0\n");
        assert!(!settings.enabled);
        assert_eq!(warnings, Vec::<String>::new());
    }

    /// Every key takes effect.
    #[test]
    fn every_key_is_read() {
        let (settings, warnings) = parse(
            "[Climb]\nEnabled=0\nSurfaceMask=0x104\nPlaneTolerance=0.3\nStaminaMultiplier=2.5\n\
             [Start]\nWalk=0\nJump=0\nSprint=0\nGlide=1\n\
             [Tuning]\nWalkableAngle=45\nOverhangAngle=100\nEntryReach=1.1\nHoldReach=1.2\n\
             GraceSeconds=0.3\n\
             [Debug]\nDiagnostics=0\n",
        );
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(
            settings,
            Settings {
                enabled: false,
                surface_mask: 0x104,
                plane_tolerance: 0.3,
                stamina_multiplier: 2.5,
                walk: false,
                jump: false,
                sprint: false,
                glide: true,
                walkable_angle: 45.0,
                overhang_angle: 100.0,
                entry_reach: 1.1,
                hold_reach: 1.2,
                grace_seconds: 0.3,
                diagnostics: false,
            }
        );
    }

    /// Decimal and either case of hex work.
    #[test]
    fn the_mask_can_be_written_in_decimal_or_hex() {
        assert_eq!(mask_of("292"), Some(0x124));
        assert_eq!(mask_of("0x124"), Some(0x124));
        assert_eq!(mask_of("0X12C"), Some(0x12c));
        assert_eq!(mask_of(" 0x4 "), Some(0x4));
    }

    /// Bits 0 and 1 give a different surface per ray, and bit 4 is water. They are refused, with
    /// a warning, rather than climbed badly.
    #[test]
    fn a_mask_with_bad_bits_warns_and_uses_the_default() {
        for text in [
            "0", "0x0", "0x12f", "0x1", "0x2", "0x10", "rock", "0x", "-1",
        ] {
            let (settings, warnings) = parse(&format!("[Climb]\nSurfaceMask={text}\n"));
            assert_eq!(settings.surface_mask, 0x124, "{text}");
            assert_eq!(warnings.len(), 1, "{text}: {warnings:?}");
            assert!(warnings[0].contains("SurfaceMask"), "{warnings:?}");
        }
    }

    /// An out-of-range number moves to the nearest limit and says so.
    #[test]
    fn a_number_out_of_range_is_moved_to_the_limit() {
        let (settings, warnings) = parse("[Climb]\nPlaneTolerance=9\nStaminaMultiplier=-1\n");
        assert_eq!(settings.plane_tolerance, 1.0);
        assert_eq!(settings.stamina_multiplier, 0.0);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
    }

    /// A bad value warns and uses its default.
    #[test]
    fn a_bad_value_warns_and_uses_the_default() {
        let (settings, warnings) = parse("[Start]\nWalk=maybe\n[Tuning]\nEntryReach=far\n");
        assert_eq!(settings, Settings::default());
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("Walk"), "{warnings:?}");
        assert!(warnings[1].contains("EntryReach"), "{warnings:?}");
    }

    /// A typo must not pass in silence.
    #[test]
    fn an_unknown_key_warns() {
        let (_, warnings) = parse("[Climb]\nEnabledd=1\n[Rock]\nX=1\n");
        assert_eq!(warnings.len(), 2, "{warnings:?}");
    }

    /// A file that cannot be parsed gives the defaults and one warning.
    #[test]
    fn a_broken_file_gives_the_defaults() {
        let (settings, warnings) = parse("[Climb\nEnabled=0\n");
        assert_eq!(settings, Settings::default());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    /// The schema and the default file list the same keys, and each schema default is the value
    /// the file has. This is what keeps the two from drifting.
    #[test]
    fn the_schema_matches_the_default_ini() {
        let mut ini = Ini::new();
        ini.read(DEFAULT_INI.to_owned()).unwrap();
        let map = ini.get_map_ref();
        let in_file: usize = map.values().map(|keys| keys.len()).sum();
        assert_eq!(in_file, schema().len());
        for setting in schema() {
            let value = ini.get(setting.section, setting.key);
            assert_eq!(
                value.as_deref(),
                setting.default,
                "{}.{}",
                setting.section,
                setting.key
            );
        }
    }

    /// Each number in the schema has the range the reader uses.
    #[test]
    fn the_schema_ranges_are_the_readers() {
        for setting in schema() {
            let SettingType::Float { min, max, .. } = setting.kind else {
                continue;
            };
            let below = format!("[{}]\n{}={}\n", setting.section, setting.key, min - 1.0);
            let above = format!("[{}]\n{}={}\n", setting.section, setting.key, max + 1.0);
            let (_, low) = parse(&below);
            let (_, high) = parse(&above);
            assert_eq!(low.len(), 1, "{}: {low:?}", setting.key);
            assert_eq!(high.len(), 1, "{}: {high:?}", setting.key);
            assert!(low[0].contains(&format!("using {min}")), "{low:?}");
            assert!(high[0].contains(&format!("using {max}")), "{high:?}");
        }
    }

    /// The development overhang setting accepts the requested range without changing defaults.
    #[test]
    fn overhang_parser_accepts_160_and_keeps_the_110_default() {
        assert_eq!(Settings::default().overhang_angle, 110.0);
        for angle in [135.0, 145.0, 155.0, 160.0] {
            let (settings, warnings) = parse(&format!("[Tuning]\nOverhangAngle={angle}\n"));
            assert_eq!(settings.overhang_angle, angle, "{warnings:?}");
            assert!(warnings.is_empty(), "{angle}: {warnings:?}");
        }
        let (settings, warnings) = parse("[Tuning]\nOverhangAngle=175\n");
        assert_eq!(settings.overhang_angle, 160.0);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("using 160"), "{warnings:?}");
    }

    /// The angles turn into the limits the surface code compares with.
    #[test]
    fn the_tuning_follows_the_angles() {
        let t = Settings::default().tuning();
        assert!((t.walkable_ny - 50f32.to_radians().cos()).abs() < 1e-6);
        assert!(t.hold_ny > t.walkable_ny);
        assert!((t.overhang_ny - 110f32.to_radians().cos()).abs() < 1e-6);
        assert!(t.overhang_ny < 0.0);
        assert!((t.move_overhang_ny - 125f32.to_radians().cos()).abs() < 1e-6);
        assert_eq!(t.face, crate::exit::FaceRay::new(50.0));
        assert_eq!(t.mask, 0x124);
    }

    /// The default rays reach the shallowest face that is climbable.
    #[test]
    fn the_entry_rays_reach_the_shallowest_climbable_face() {
        let t = Settings::default().tuning();
        assert!(t.entry_ray > 1.9, "{t:?}");
    }

    /// The grace time stays in seconds in the file and becomes ticks at 60 a second.
    #[test]
    fn the_grace_time_becomes_ticks() {
        assert_eq!(Settings::default().grace_ticks(), 0);
        let ticks = |grace_seconds| {
            let settings = Settings {
                grace_seconds,
                ..Settings::default()
            };
            settings.grace_ticks()
        };
        assert_eq!(ticks(0.0), 0);
        assert_eq!(ticks(0.05), 3);
        assert_eq!(ticks(1.0), 60);
    }

    /// A missing file is not an error, and the default is written once, never over a player's
    /// file.
    #[test]
    fn the_default_file_is_written_once() {
        let dir = std::env::temp_dir().join(format!("ember-climb-ini-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(INI_FILE_NAME);
        let _ = std::fs::remove_file(&file);
        assert!(read_ini(&dir).unwrap().is_none());
        write_default(&dir).unwrap();
        assert_eq!(read_ini(&dir).unwrap().unwrap().0, Settings::default());
        assert!(write_default(&dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
