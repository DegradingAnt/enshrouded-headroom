//! Reusable INI reader and warning rules.
use configparser::ini::Ini;
use std::collections::HashSet;
#[cfg(feature = "test-support")]
thread_local! { static REQUESTED: std::cell::RefCell<std::collections::BTreeSet<(String,String)>> = const { std::cell::RefCell::new(std::collections::BTreeSet::new()) }; }
/// Take the current thread's requested-key inventory, for cross-crate parser coverage tests.
#[cfg(feature = "test-support")]
pub fn take_requested_keys() -> std::collections::BTreeSet<(String, String)> {
    REQUESTED.with(|keys| std::mem::take(&mut *keys.borrow_mut()))
}
/// The most characters of the player's own text that one warning repeats.
pub const MAX_ECHO_CHARS: usize = 64;

/// `text`, cut to `MAX_ECHO_CHARS` characters (on a character boundary) with `…` added if it was
/// cut. A value or key can be as long as the file, and a log line must not be.
pub fn shorten(text: &str) -> String {
    match text.char_indices().nth(MAX_ECHO_CHARS) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// A number as a person types it. A decimal comma (`0,9`) counts. `nan` and `inf` do not.
pub fn parse_number(text: &str) -> Option<f64> {
    let value: f64 = text.trim().replace(',', ".").parse().ok()?;
    value.is_finite().then_some(value)
}

/// The parsed file plus the warnings so far. The typed getters mark each key they ask for, so the
/// keys left over at the end are the unknown ones.
pub struct Reader {
    ini: Ini,
    /// Lower-case `(section, key)` pairs that a getter has asked for.
    used: HashSet<(String, String)>,
    warnings: Vec<String>,
}

impl Reader {
    /// Begin collecting warnings for an owned parsed file.
    pub fn new(ini: Ini) -> Self {
        Self {
            ini,
            used: HashSet::new(),
            warnings: Vec::new(),
        }
    }
    /// Append a mod-specific warning in parsing order.
    pub fn warn(&mut self, warning: String) {
        self.warnings.push(warning);
    }
    /// Accumulated warning text.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
    /// Finish and return warnings.
    pub fn into_warnings(self) -> Vec<String> {
        self.warnings
    }

    /// The text of `[section] key`, or `None` if the file does not have it. A key written without
    /// `=` has no text, which reads as an empty value.
    pub fn raw(&mut self, section: &str, key: &str) -> Option<String> {
        #[cfg(feature = "test-support")]
        REQUESTED.with(|keys| {
            keys.borrow_mut()
                .insert((section.to_lowercase(), key.to_lowercase()));
        });
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
    pub fn not_valid(
        &mut self,
        section: &str,
        key: &str,
        text: &str,
        expected: &str,
        default: &str,
    ) {
        let text = shorten(text);
        self.warnings.push(format!(
            "[{section}] {key}: \"{text}\" is not {expected}, using the default {default}"
        ));
    }

    /// Clamp into `lo..=hi`, with a warning if that changed the value.
    pub fn clamp(&mut self, section: &str, key: &str, value: f64, lo: f64, hi: f64) -> f64 {
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

    /// Read a bounded finite floating-point value.
    pub fn float(&mut self, section: &str, key: &str, default: f64, lo: f64, hi: f64) -> f64 {
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
    pub fn int(
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

    /// Read a boolean with the original warning rules.
    pub fn boolean(&mut self, section: &str, key: &str, default: bool) -> bool {
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
    pub fn choice<T: Copy + PartialEq>(
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

    /// Warn about each section and key that no getter asked for.
    pub fn report_unknown(&mut self, sections: &[&str]) {
        let mut unknown = Vec::new();
        for (section, keys) in self.ini.get_map_ref() {
            let Some(known) = sections.iter().find(|s| s.to_lowercase() == *section) else {
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

/// The value domain of one setting, including optional numeric word alternatives.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SettingType {
    /// A boolean accepting 0, 1, true and false.
    Bool,
    /// A bounded whole number, optionally accepting named alternatives.
    Int {
        /// Inclusive minimum.
        min: u32,
        /// Inclusive maximum.
        max: u32,
        /// Whether zero is separately allowed.
        zero: bool,
        /// Named alternatives.
        words: &'static [&'static str],
    },
    /// A bounded finite number, optionally accepting named alternatives.
    Float {
        /// Inclusive minimum.
        min: f64,
        /// Inclusive maximum.
        max: f64,
        /// Named alternatives.
        words: &'static [&'static str],
    },
    /// A closed list of case-insensitive words.
    Choice(&'static [&'static str]),
    /// Unrestricted text.
    Text,
}
/// File-independent metadata supplied by a mod for one setting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Setting {
    /// INI section name.
    pub section: &'static str,
    /// INI key name.
    pub key: &'static str,
    /// Accepted value domain.
    pub kind: SettingType,
    /// Default file spelling; None means absent, not a literal word.
    pub default: Option<&'static str>,
    /// One-line description of the setting.
    pub description: &'static str,
    /// Whether changes need the process to restart.
    pub needs_restart: bool,
}
