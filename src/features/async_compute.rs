//! Async compute, through the game's own override path.
//!
//! While the game sets up its renderer it works out an automatic async-compute flag (in `cl`).
//! If the command line gave `--enable-async-compute` or `--disable-async-compute`, the game then
//! stores the player's choice for the renderer in place of the automatic value. The code, in build
//! f68140a3 at `0x1408490d2` (offsets are from the start of `ASYNC_OVERRIDE`'s match):
//!
//! ```text
//! +0   88 4d 3e              mov   [rbp+0x3e], cl            ; the automatic value
//! +3   44 38 b6 89 52 03 00  cmp   [rsi+0x35289], r14b       ; did the command line set it?
//! +10  74 0a                 je    +22
//! +12  0f b6 86 88 52 03 00  movzx eax, byte [rsi+0x35288]   ; the player's value
//! +19  88 45 3e              mov   [rbp+0x3e], al
//! +22  88 8d 98 0a 00 00     mov   [rbp+0xa98], cl           ; the automatic value again
//! ```
//!
//! The patch writes [`NEW`] over the 16 bytes at +3: `mov al, 1` and two NOPs (8 and 6 bytes).
//! The code then runs into `mov [rbp+0x3e], al` at +19 with `al` = 1, which stores what
//! `--enable-async-compute` stores. `cl` is not changed, so the automatic value that the store
//! at +22 keeps (and that the tier flags are chosen from) is as before.
//!
//! The code realigns at +19. A scan of the 8017 `.pdata` chunks within 1 MiB found no direct
//! branch into +4..+18; indirect jumps (jump tables) were not checked.
//!
//! The patch also removes the `cmp` (flags) and the `movzx` (`eax`). Nothing reads them: after
//! the patch, the only instructions that run before `call 0x140e28450` are `mov [rbp+0xa98], cl`
//! and `mov rcx, [rsi+0x5a8]` (decoded from the bytes, controller, 2026-10-26). Under the Win64
//! calling convention the flags and the upper bits of `rax` are dead across that call. Caveat:
//! whether the callee takes a custom calling convention is not known.
//!
//! # When it runs
//!
//! [`init`], from `entry::start`, once, before the game's threads run. It patches only when
//! [`decide`] says [`Decision::Patch`] (checked in this order):
//!
//! 1. code patches are allowed for this build (`entry::code_patches_allowed`);
//! 2. the settings file does not say `AsyncCompute = game`;
//! 3. no argument of the command line mentions `async-compute` (see [`decide`]).
//!
//! [`apply`] finds `ASYNC_OVERRIDE` (it must match exactly once), adds the patch offset 3 and
//! calls `write_checked` with all 16 [`ORIGINAL`] bytes. That includes +11, the `je`
//! displacement, which the signature leaves as a wildcard and the scan therefore does not prove.
//! Bytes that are not the original (another tool, another build) are left alone and logged.
//!
//! # Other tools
//!
//! The 2026-10-23 launcher passes `--enable-async-compute`. With it still in use the command line
//! sets the flag, [`decide`] skips, and the game does what the launcher asked. A second run of
//! [`apply`] does not find the signature (it covers the patched bytes) and writes nothing; the log
//! says the game code was not found, whichever tool changed it.

use log::Level;
use windows_sys::Win32::System::Environment::GetCommandLineW;

use crate::config::AsyncCompute;
use crate::entry;
use crate::patch::{write_checked, PatchError};
use crate::scan::{find_in_sections, main_module, Module, Pattern, ScanError};
use crate::sigs::ASYNC_OVERRIDE;

/// The 16 bytes at +3 of the signature's match, as the game has them: `cmp [rsi+0x35289], r14b`,
/// `je +10` (displacement `0x0a`), `movzx eax, byte [rsi+0x35288]`.
pub const ORIGINAL: [u8; 16] = [
    0x44, 0x38, 0xb6, 0x89, 0x52, 0x03, 0x00, 0x74, 0x0a, 0x0f, 0xb6, 0x86, 0x88, 0x52, 0x03, 0x00,
];

/// What they become: `mov al, 1`, `nop dword [rax+rax+0]` (8 bytes), `nop word [rax+rax+0]`
/// (6 bytes). The next instruction, `mov [rbp+0x3e], al`, is not touched.
pub const NEW: [u8; 16] = [
    0xb0, 0x01, 0x0f, 0x1f, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00, 0x66, 0x0f, 0x1f, 0x44, 0x00, 0x00,
];

/// What the game's own flags, `--enable-async-compute` and `--disable-async-compute`, have in
/// common, in lower case. An argument that contains it is taken to set async compute (see
/// [`sets_async_compute`]).
const MARKER: &str = "async-compute";

/// Whether to patch, and if not, why not (the reason goes into the log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Patch,
    Skip(&'static str),
}

/// A blank to Windows' command-line parser: a space or a tab.
fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// `cmdline` without its first token, the program. A program name in double quotes runs to the
/// next quote (or to the end of the line if there is none), so spaces inside it do not split it.
/// Blanks at the start are not skipped: a command line that starts with a blank has an empty
/// program name, and its first word is an argument (as `CommandLineToArgvW`, the UCRT and Rust's
/// `std` read it).
fn arguments(cmdline: &str) -> &str {
    match cmdline.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"').map_or("", |(_, rest)| rest),
        None => cmdline.split_once(is_blank).map_or("", |(_, rest)| rest),
    }
}

/// Does an argument of `cmdline` contain `async-compute`, ignoring ASCII case and with every
/// double quote removed from the argument? The program name is never an argument.
///
/// The game's own flags are the ASCII words `--enable-async-compute` and
/// `--disable-async-compute`, but its parser's rules for case and prefix were not traced. So this
/// takes more than those two exact words: `--disable-async-compute=1`, `-Disable-Async-Compute`
/// and `--disable-"async"-compute` count too. A skip is the safe direction: the game keeps its
/// own choice.
fn sets_async_compute(cmdline: &str) -> bool {
    arguments(cmdline).split(is_blank).any(|token| {
        let bare: String = token.chars().filter(|&c| c != '"').collect();
        bare.to_ascii_lowercase().contains(MARKER)
    })
}

/// Should async compute be patched? The checks run in this order, and the first that fails gives
/// the reason:
///
/// 1. `code_patches == false` gives `"unknown build"`;
/// 2. `AsyncCompute = game` gives `"ini: game"`;
/// 3. an argument of the command line that contains `async-compute` (see
///    `sets_async_compute`) gives `"command line sets it"`. The program name is not an
///    argument.
///
/// Otherwise `Patch`. Pure: the command line is passed in (see `command_line`).
pub fn decide(cmdline: &str, cfg: &AsyncCompute, code_patches: bool) -> Decision {
    if !code_patches {
        return Decision::Skip("unknown build");
    }
    match cfg {
        AsyncCompute::Game => return Decision::Skip("ini: game"),
        AsyncCompute::On => {}
    }
    if sets_async_compute(cmdline) {
        return Decision::Skip("command line sets it");
    }
    Decision::Patch
}

/// The most UTF-16 units Windows allows in a command line, not counting the NUL.
const MAX_COMMAND_LINE: usize = 32767;

/// This process's command line (`GetCommandLineW`), decoded lossily from UTF-16.
fn command_line() -> String {
    // SAFETY: `GetCommandLineW` has no preconditions. It returns a pointer to the process's
    // command line, a NUL-terminated UTF-16 string (a `WCHAR` buffer, so 2-byte aligned as `u16`
    // needs) that lives as long as the process.
    let start = unsafe { GetCommandLineW() };
    if start.is_null() {
        return String::new();
    }
    let mut len = 0;
    // SAFETY: `start` is not null and points to a NUL-terminated string that does not change; it
    // is a `WCHAR` buffer, so it is 2-byte aligned as a `u16` read needs, and so is every
    // `start.add(len)`. The read at `len` happens only after units `0..len` were all non-NUL, so
    // it is at or before the NUL and inside the string. `MAX_COMMAND_LINE` is the longest string
    // Windows allows, a second guard against a string that has no end.
    while len < MAX_COMMAND_LINE && unsafe { *start.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: the `len` units from `start` were all read above, so they are valid and readable;
    // `start` is 2-byte aligned (a `WCHAR` buffer, as above); `u16` has no validity requirement,
    // and the string is not written meanwhile.
    let units = unsafe { std::slice::from_raw_parts(start, len) };
    String::from_utf16_lossy(units)
}

/// Replace [`ORIGINAL`] with [`NEW`] at `site`, if memory holds all 16 original bytes.
///
/// # Safety
///
/// The contract of [`write_checked`] for the 16 bytes at `site`: they are code that no thread is
/// running. So: start-up only, before the game's threads run.
unsafe fn write_site(site: usize) -> Result<(), PatchError> {
    // SAFETY: the caller's contract for `write_site` is the contract of `write_checked`.
    // `ORIGINAL` and `NEW` are constants in this module's data, not the target.
    unsafe { write_checked(site, &ORIGINAL, &NEW) }.map(|_| ())
}

/// The log text for a patch that failed at `site`, starting with what became of the feature.
fn failure_text(site: usize, error: PatchError) -> String {
    match error {
        PatchError::Mismatch { at, found } => format!(
            "off: the code at {site:#x} is not the original (byte {at:#x} differs; read \
             {found:02x?}); another tool or a different build changed it, so it is left alone"
        ),
        PatchError::ReadBack => format!(
            "patch written at {site:#x}, but the read-back did not match; the code there is \
             not what was written"
        ),
        other => format!("off: cannot patch the code at {site:#x}: {other}"),
    }
}

/// Find `ASYNC_OVERRIDE` in `m`, add its patch offset (3) and write [`NEW`] over [`ORIGINAL`].
///
/// The signature must match exactly once. Every one of the 16 bytes is compared before anything
/// is written, so bytes that differ (including the one the signature does not look at) leave the
/// code as it is. `Err` is the reason for the log, and starts with what became of the feature
/// (`off: ...`) except for a read-back failure, where the bytes were written.
///
/// A signature that is not found says "the game code was not found (another tool or a different
/// build may have changed it)": it is also what a second run, or an earlier tool that turned
/// async compute on, leaves behind. The scan's own error goes to the debug log.
///
/// # Safety
///
/// Start-up only, before the game's threads run the code that is patched: `find_in_sections`
/// reads the image in place and `write_checked` changes code, and both rely on no other thread
/// writing or running it. Call it from `entry::start`, and only after [`decide`] said
/// [`Decision::Patch`]. `m` must describe mapped, readable memory of the module (as
/// `main_module()` does).
pub unsafe fn apply(m: &Module) -> Result<(), String> {
    let name = ASYNC_OVERRIDE.name;
    let pattern =
        Pattern::parse(ASYNC_OVERRIDE.pattern).map_err(|error| format!("off: {name}: {error}"))?;
    let found = find_in_sections(m, ASYNC_OVERRIDE.sections, &pattern).map_err(|error| {
        log::debug!("async compute: {name}: {error}");
        scan_failure_text(&error)
    })?;
    let site = found
        .checked_add(ASYNC_OVERRIDE.patch_offset())
        .ok_or_else(|| format!("off: {name}: the patch site is past the end of memory"))?;
    log::debug!("async compute: {name} matched at {found:#x}, patching {site:#x}");
    // SAFETY: the caller's contract for `apply`: start-up only, so no thread runs the code at
    // `site`. The 16 bytes at `site` end at the end of the 19-byte match `find_in_sections` just
    // found in `.text`, in the mapped, readable memory of `m`.
    unsafe { write_site(site) }.map_err(|error| failure_text(site, error))
}

/// The log text for a signature that did not match exactly once, starting with what became of
/// the feature.
fn scan_failure_text(error: &ScanError) -> String {
    match error {
        ScanError::Multiple(count) => format!(
            "off: the game code matched {count} times, not once (another tool or a different \
             build may have changed it)"
        ),
        ScanError::NotFound | ScanError::BadPattern(_) => {
            "off: the game code was not found (another tool or a different build may have \
             changed it)"
                .to_owned()
        }
    }
}

/// The body of [`init`], with the command line, the code-patch answer and the patching passed in.
/// `apply` runs only after [`Decision::Patch`]. Returns the log level and the one line to log.
fn start_up(
    cfg: &AsyncCompute,
    cmdline: &str,
    code_patches: bool,
    apply: impl FnOnce() -> Result<(), String>,
) -> (Level, String) {
    match decide(cmdline, cfg, code_patches) {
        Decision::Skip(reason) => (Level::Info, format!("async compute: off: {reason}")),
        Decision::Patch => match apply() {
            Ok(()) => (Level::Info, "async compute: on".to_owned()),
            Err(why) => (Level::Warn, format!("async compute: {why}")),
        },
    }
}

/// The start-up step: decide, patch if it is right to, and log one line. Called once, from
/// `entry::start`, after `code_patches_allowed` is set, so it may scan and patch the exe (see
/// [`apply`]).
pub fn init(cfg: &AsyncCompute) {
    let (level, line) = start_up(cfg, &command_line(), entry::code_patches_allowed(), || {
        // SAFETY: `init` is called once from `entry::start`, at start-up and before the game's
        // threads run the code that is patched (the start-up-only rule in `apply`'s `# Safety`),
        // and `start_up` calls this only after `decide` said `Patch`. `main_module()` is the
        // running exe: mapped, readable memory.
        unsafe { apply(&main_module()) }
    });
    log::log!(level, "{line}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::test_support::Mem;
    use crate::scan::{find_in_sections, main_module, Pattern};
    use crate::sigs::ASYNC_OVERRIDE;
    use std::cell::Cell;

    // ---- the values the plan gives verbatim ---------------------------------------------------

    #[test]
    fn patches_by_default() {
        assert!(matches!(
            decide("enshrouded.exe", &AsyncCompute::On, true),
            Decision::Patch
        ));
    }

    #[test]
    fn respects_command_line() {
        assert!(matches!(
            decide(
                r#""C:\x\enshrouded.exe" --Disable-Async-Compute"#,
                &AsyncCompute::On,
                true
            ),
            Decision::Skip(_)
        ));
        assert!(matches!(
            decide(
                "enshrouded.exe --enable-async-compute",
                &AsyncCompute::On,
                true
            ),
            Decision::Skip(_)
        ));
    }

    #[test]
    fn respects_ini_and_build() {
        assert!(matches!(
            decide("x", &AsyncCompute::Game, true),
            Decision::Skip(_)
        ));
        assert!(matches!(
            decide("x", &AsyncCompute::On, false),
            Decision::Skip(_)
        ));
    }

    #[test]
    fn new_bytes_decode_as_mov_al_1_and_nops() {
        // NEW is 2 + 8 + 6 bytes; the instruction after it (mov [rbp+0x3e], al) is untouched
        assert_eq!(NEW.len(), ORIGINAL.len());
        assert_eq!(&NEW[..2], &[0xb0, 0x01]);
        // The rest, checked without a decoder: `nop dword [rax+rax+0]` (8 bytes), then
        // `nop word [rax+rax+0]` (6 bytes).
        assert_eq!(&NEW[2..10], &[0x0f, 0x1f, 0x84, 0, 0, 0, 0, 0]);
        assert_eq!(&NEW[10..16], &[0x66, 0x0f, 0x1f, 0x44, 0, 0]);
    }

    // ---- the exact reasons, and the order they are checked in --------------------------------

    #[test]
    fn each_skip_gives_its_own_reason() {
        let flagged = "enshrouded.exe --enable-async-compute";
        assert_eq!(
            decide(flagged, &AsyncCompute::On, true),
            Decision::Skip("command line sets it")
        );
        assert_eq!(
            decide("x", &AsyncCompute::Game, true),
            Decision::Skip("ini: game")
        );
        assert_eq!(
            decide("x", &AsyncCompute::On, false),
            Decision::Skip("unknown build")
        );
    }

    #[test]
    fn the_checks_run_in_order_build_then_ini_then_command_line() {
        let flagged = "enshrouded.exe --disable-async-compute";
        // All three say skip: the build comes first.
        assert_eq!(
            decide(flagged, &AsyncCompute::Game, false),
            Decision::Skip("unknown build")
        );
        // Known build, ini and command line both say skip: the ini comes first.
        assert_eq!(
            decide(flagged, &AsyncCompute::Game, true),
            Decision::Skip("ini: game")
        );
        assert_eq!(
            decide(flagged, &AsyncCompute::On, true),
            Decision::Skip("command line sets it")
        );
    }

    // ---- reading the command line ------------------------------------------------------------

    const SPACED_EXE: &str =
        r#""C:\Program Files (x86)\Steam\steamapps\common\Enshrouded\enshrouded.exe""#;

    #[test]
    fn an_exe_path_with_spaces_and_quotes_is_the_program_not_an_argument() {
        assert_eq!(decide(SPACED_EXE, &AsyncCompute::On, true), Decision::Patch);
        let with_flag = format!("{SPACED_EXE} --disable-async-compute");
        assert_eq!(
            decide(&with_flag, &AsyncCompute::On, true),
            Decision::Skip("command line sets it")
        );
        let with_other_args = format!("{SPACED_EXE} -windowed --enable-dx12 -nosplash");
        assert_eq!(
            decide(&with_other_args, &AsyncCompute::On, true),
            Decision::Patch
        );
    }

    #[test]
    fn async_compute_in_the_program_name_does_not_count() {
        for cmd in [
            // Quoted, with blanks inside the quotes that would cut the path into words.
            r#""C:\a --disable-async-compute b\enshrouded.exe""#,
            r#""C:\a --enable-async-compute b\enshrouded.exe" -windowed"#,
            // The flag spelled as part of a path, quoted or not.
            r"C:\--disable-async-compute\x.exe",
            r"C:\--enable-async-compute\x.exe",
            r#""C:\--disable-async-compute\x.exe" -windowed"#,
            r"C:\async-compute\x.exe -windowed",
        ] {
            assert_eq!(
                decide(cmd, &AsyncCompute::On, true),
                Decision::Patch,
                "{cmd}"
            );
        }
    }

    #[test]
    fn any_argument_that_mentions_async_compute_counts() {
        for cmd in [
            // A path given as an argument is an argument (T8-7).
            r"x.exe C:\--disable-async-compute\x.exe",
            r#"x.exe "D:\data\--enable-async-compute\save.dat""#,
            // Something glued to either end of a flag, or another prefix for it.
            "x.exe --disable-async-computes",
            "x.exe --disable-async-compute-now",
            "x.exe x--enable-async-compute",
            "x.exe ---enable-async-compute",
            "x.exe --enable-async-compute=1",
            "x.exe --disable-async-compute=1",
            "x.exe --async-compute",
            "x.exe -enable-async-compute",
            "x.exe -Disable-Async-Compute",
            "x.exe /enable-async-compute",
            // Quotes inside the word.
            r#"x.exe --disable-"async"-compute"#,
            r#"x.exe "--disable-async-compute"#,
            r#"x.exe --disable-async-compute""#,
        ] {
            assert_eq!(
                decide(cmd, &AsyncCompute::On, true),
                Decision::Skip("command line sets it"),
                "{cmd}"
            );
        }
    }

    #[test]
    fn arguments_that_only_resemble_the_flags_do_not_count() {
        for cmd in [
            "x.exe --disable-async",
            "x.exe --async_compute",
            "x.exe --disable-async-comput",
            "x.exe --asynccompute",
            "x.exe --compute-async",
            // Two words, neither of which has it.
            "x.exe --disable-async -compute",
        ] {
            assert_eq!(
                decide(cmd, &AsyncCompute::On, true),
                Decision::Patch,
                "{cmd}"
            );
        }
    }

    #[test]
    fn a_command_line_that_starts_with_a_blank_has_an_empty_program_name() {
        // The first word is then an argument, as CommandLineToArgvW, the UCRT and Rust's std read
        // it (T8-1): it is not skipped as the program.
        assert_eq!(arguments(" a b"), "a b");
        assert_eq!(arguments("\ta"), "a");
        assert_eq!(arguments("  a"), " a");
        assert_eq!(arguments(" "), "");
        assert_eq!(arguments("a b c"), "b c");
        assert_eq!(arguments(r#""a b" c"#), " c");
        assert_eq!(arguments(""), "");
        for cmd in [
            " --disable-async-compute",
            "\t--enable-async-compute",
            "  x.exe --disable-async-compute",
        ] {
            assert_eq!(
                decide(cmd, &AsyncCompute::On, true),
                Decision::Skip("command line sets it"),
                "{cmd:?}"
            );
        }
        // No argument that mentions it, so nothing to skip.
        for cmd in [" x.exe", " ", "  "] {
            assert_eq!(decide(cmd, &AsyncCompute::On, true), Decision::Patch);
        }
    }

    #[test]
    fn the_first_token_is_the_program_and_is_ignored() {
        // The ruling: ignore the first token, even if it is spelled like a flag.
        assert_eq!(
            decide("--disable-async-compute", &AsyncCompute::On, true),
            Decision::Patch
        );
        assert_eq!(
            decide(
                r#""--enable-async-compute" -windowed"#,
                &AsyncCompute::On,
                true
            ),
            Decision::Patch
        );
    }

    #[test]
    fn tokens_are_cut_at_spaces_and_tabs_and_matched_without_case_or_quotes() {
        for cmd in [
            "x.exe\t--disable-async-compute",
            "x.exe   --ENABLE-ASYNC-COMPUTE   ",
            "x.exe -windowed --Enable-Async-Compute -nosplash",
            r#"x.exe "--disable-async-compute""#,
            r#"x.exe -windowed "--Enable-Async-Compute" -nosplash"#,
            // No space between the closing quote of the program and the first argument.
            r#""x.exe"--disable-async-compute"#,
        ] {
            assert_eq!(
                decide(cmd, &AsyncCompute::On, true),
                Decision::Skip("command line sets it"),
                "{cmd:?}"
            );
        }
    }

    #[test]
    fn odd_command_lines_patch_and_do_not_panic() {
        for cmd in [
            "",
            " ",
            "\"",
            "\"\"",
            "x.exe \"",
            "x.exe \"\" \"\"",
            // An unterminated quote makes the rest of the line part of the program name, as
            // Windows reads it.
            r#""C:\x\enshrouded.exe --disable-async-compute"#,
            "x.exe \u{1f600} \u{fffd}",
        ] {
            assert_eq!(
                decide(cmd, &AsyncCompute::On, true),
                Decision::Patch,
                "{cmd:?}"
            );
        }
    }

    #[test]
    fn the_real_command_line_is_this_process_command_line() {
        let cmd = command_line();
        // The test binary is `enshrouded_headroom-<hash>.exe`.
        assert!(
            cmd.to_ascii_lowercase().contains("enshrouded_headroom"),
            "{cmd}"
        );
        assert!(!cmd.contains('\0'), "{cmd:?}");

        // This module's own `arguments` against what the standard library reports (T8-6). The
        // program name comes before the arguments, and is argv[0].
        let args = arguments(&cmd);
        assert!(cmd.ends_with(args), "{cmd}");
        let program = &cmd[..cmd.len() - args.len()];
        let exe = std::env::args().next().expect("argv[0]");
        assert!(
            program.contains(&exe),
            "program {program:?}, argv[0] {exe:?}"
        );
        assert!(!args.contains(&exe), "{args:?} must not hold the program");

        // The harness's own arguments (`--exact`, a test filter, ...) are plain words. When they
        // are (no blank or quote in any), they are the words of `arguments`, in order.
        let rest: Vec<String> = std::env::args().skip(1).collect();
        if rest.iter().all(|a| !a.contains([' ', '\t', '"'])) {
            let words: Vec<String> = args
                .split(is_blank)
                .filter(|word| !word.is_empty())
                .map(|word| word.replace('"', ""))
                .collect();
            assert_eq!(words, rest, "{cmd}");
        }
    }

    // ---- the bytes ---------------------------------------------------------------------------

    /// The pattern as bytes: `None` for `??`.
    fn signature() -> Vec<Option<u8>> {
        ASYNC_OVERRIDE
            .pattern
            .split_ascii_whitespace()
            .map(|t| u8::from_str_radix(t, 16).ok())
            .collect()
    }

    #[test]
    fn original_is_the_signature_bytes_and_the_one_wildcard_is_the_je_displacement() {
        let sig = signature();
        let at = ASYNC_OVERRIDE.patch_offset();
        assert_eq!(at, 3);
        // The patch ends where the pattern ends: the next instruction starts at +19.
        assert_eq!(sig.len(), at + ORIGINAL.len());
        let wild: Vec<usize> = sig
            .iter()
            .enumerate()
            .filter_map(|(i, b)| b.is_none().then_some(i))
            .collect();
        assert_eq!(wild, [at + 8], "one wildcard, the `je` displacement at +11");
        for (i, want) in ORIGINAL.iter().enumerate() {
            match sig[at + i] {
                Some(got) => assert_eq!(got, *want, "byte +{}", at + i),
                None => assert_eq!(*want, 0x0a, "the displacement is `je +22`"),
            }
        }
        // cmp [rsi+0x35289], r14b / je +10 / movzx eax, byte [rsi+0x35288]
        assert_eq!(&ORIGINAL[..7], &[0x44, 0x38, 0xb6, 0x89, 0x52, 0x03, 0x00]);
        assert_eq!(&ORIGINAL[7..9], &[0x74, 0x0a]);
        assert_eq!(&ORIGINAL[9..], &[0x0f, 0xb6, 0x86, 0x88, 0x52, 0x03, 0x00]);
        // The bytes before the patch are `mov [rbp+0x3e], cl`.
        assert_eq!(&sig[..3], &[Some(0x88), Some(0x4d), Some(0x3e)]);
    }

    // ---- writing -----------------------------------------------------------------------------

    /// `mov [rbp+0x3e], cl`, the 16 patched bytes, `mov [rbp+0x3e], al`, `mov [rbp+0xa98], cl`:
    /// the code around the site, as the game has it.
    fn site_code() -> Vec<u8> {
        let mut code = vec![0x88, 0x4d, 0x3e];
        code.extend_from_slice(&ORIGINAL);
        code.extend_from_slice(&[0x88, 0x45, 0x3e]);
        code.extend_from_slice(&[0x88, 0x8d, 0x98, 0x0a, 0x00, 0x00]);
        code
    }

    #[test]
    fn write_site_changes_the_sixteen_bytes_and_nothing_else() {
        let mem = Mem::rw(0x1000);
        let code = site_code();
        mem.put(0x100, &code);
        // SAFETY: `mem` is read-write memory of this test; no thread runs the bytes in it.
        unsafe { write_site(mem.base + 0x103) }.expect("original bytes are there");
        let got = mem.get(0x100, code.len());
        assert_eq!(&got[..3], &code[..3], "mov [rbp+0x3e], cl before the patch");
        assert_eq!(&got[3..19], &NEW);
        assert_eq!(&got[19..], &code[19..], "mov [rbp+0x3e], al and the rest");
    }

    #[test]
    fn write_site_leaves_changed_code_alone_even_in_the_one_byte_the_scan_skips() {
        // +11 of the match is the `je` displacement, a wildcard in the signature. A build that
        // changed only it still matches the signature; the write must refuse it.
        for index in [8usize, 0, 15] {
            let mem = Mem::rw(0x1000);
            let mut code = site_code();
            code[3 + index] ^= 0x01;
            mem.put(0x100, &code);
            // SAFETY: as above.
            let result = unsafe { write_site(mem.base + 0x103) };
            assert!(
                matches!(result, Err(PatchError::Mismatch { .. })),
                "index {index}: {result:?}"
            );
            assert_eq!(
                mem.get(0x100, code.len()),
                code,
                "index {index}: not written"
            );
        }
    }

    #[test]
    fn a_second_write_does_not_double_apply() {
        let mem = Mem::rw(0x1000);
        mem.put(0x100, &site_code());
        // SAFETY: as above.
        unsafe { write_site(mem.base + 0x103) }.expect("first");
        let after_first = mem.get(0x100, 0x20);
        // SAFETY: as above.
        let second = unsafe { write_site(mem.base + 0x103) };
        assert!(
            matches!(second, Err(PatchError::Mismatch { .. })),
            "{second:?}"
        );
        assert_eq!(mem.get(0x100, 0x20), after_first);
    }

    // ---- apply: find the signature, add the offset, write ------------------------------------

    /// A PE32+ header block with one section, `.text`, at RVA 0x1000, `0x1000` bytes long.
    fn text_only_header() -> Vec<u8> {
        let mut b = vec![0u8; 0x400];
        b[0..2].copy_from_slice(b"MZ");
        b[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        let fh = 0x84;
        b[fh..fh + 2].copy_from_slice(&0x8664u16.to_le_bytes());
        b[fh + 2..fh + 4].copy_from_slice(&1u16.to_le_bytes());
        b[fh + 16..fh + 18].copy_from_slice(&240u16.to_le_bytes());
        let opt = fh + 20;
        b[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        b[opt + 24..opt + 32].copy_from_slice(&0x1_4000_0000u64.to_le_bytes());
        b[opt + 56..opt + 60].copy_from_slice(&0x4000u32.to_le_bytes());
        b[opt + 60..opt + 64].copy_from_slice(&0x400u32.to_le_bytes());
        let section = opt + 240;
        b[section..section + 5].copy_from_slice(b".text");
        b[section + 8..section + 12].copy_from_slice(&0x1000u32.to_le_bytes());
        b[section + 12..section + 16].copy_from_slice(&0x1000u32.to_le_bytes());
        b[section + 16..section + 20].copy_from_slice(&0x1000u32.to_le_bytes());
        b[section + 20..section + 24].copy_from_slice(&0x400u32.to_le_bytes());
        b
    }

    /// An image of 4 read-write pages with `.text` at `0x1000`, and the module that describes it.
    fn image() -> (Mem, Module) {
        let mem = Mem::rw(0x4000);
        mem.put(0, &text_only_header());
        let module = Module {
            base: mem.base,
            size: 0x4000,
        };
        (mem, module)
    }

    /// Code around a site: filler (not the first byte of the signature), the site code, filler.
    fn function_around_site() -> Vec<u8> {
        let mut code = vec![0x90; 0x20];
        code.extend_from_slice(&site_code());
        code.extend_from_slice(&[0x90; 0x20]);
        code
    }

    /// What `apply` says when the signature is not in the code (T8-3). It does not say "no
    /// match" or name a section, and it does not blame one cause: another tool may have turned
    /// async compute on already. The scan's own error goes to the debug log.
    const NOT_FOUND: &str =
        "off: the game code was not found (another tool or a different build may have changed it)";

    /// `apply`, called as `init` calls it, on memory of this test process.
    fn apply_here(m: &Module) -> Result<(), String> {
        // SAFETY: the tests pass a `Module` for a synthetic image (read-write memory of this
        // process, described by a header the test wrote) or for the test exe itself, whose
        // `.text` has no such code, so nothing is written there. No thread runs any of the bytes
        // that can be written.
        unsafe { apply(m) }
    }

    #[test]
    fn apply_finds_the_signature_adds_three_and_writes_new() {
        let (mem, module) = image();
        let code = function_around_site();
        mem.put(0x1100, &code);
        assert_eq!(apply_here(&module), Ok(()));
        let got = mem.get(0x1100, code.len());
        // The signature starts at +0x20; the patch is 3 bytes later.
        assert_eq!(&got[..0x23], &code[..0x23], "before the patch");
        assert_eq!(&got[0x23..0x33], &NEW);
        assert_eq!(&got[0x33..], &code[0x33..], "from `mov [rbp+0x3e], al` on");
    }

    #[test]
    fn apply_twice_says_so_and_changes_nothing_more() {
        let (mem, module) = image();
        mem.put(0x1100, &function_around_site());
        assert_eq!(apply_here(&module), Ok(()));
        let after_first = mem.get(0x1000, 0x1000);
        let second = apply_here(&module).unwrap_err();
        // The signature covers the patched bytes, so once patched it is no longer found; the
        // text does not say who changed it (T8-3).
        assert_eq!(second, NOT_FOUND);
        assert_eq!(mem.get(0x1000, 0x1000), after_first);
    }

    #[test]
    fn apply_refuses_a_site_whose_unscanned_byte_differs() {
        let (mem, module) = image();
        let mut code = function_around_site();
        // +11 of the match: the `je` displacement, which the signature does not look at.
        code[0x20 + 11] = 0x0b;
        mem.put(0x1100, &code);
        // The signature still matches (that byte is a wildcard)...
        let pattern = Pattern::parse(ASYNC_OVERRIDE.pattern).unwrap();
        assert_eq!(
            find_in_sections(&module, ASYNC_OVERRIDE.sections, &pattern),
            Ok(mem.base + 0x1120)
        );
        // ...but apply compares all sixteen bytes and leaves the code as it is.
        let why = apply_here(&module).unwrap_err();
        assert!(why.contains("not the original"), "{why}");
        assert!(why.contains("left alone"), "{why}");
        assert!(why.contains(&format!("{:#x}", mem.base + 0x1123)), "{why}");
        assert_eq!(mem.get(0x1100, code.len()), code);
    }

    #[test]
    fn apply_without_the_signature_writes_nothing() {
        let (mem, module) = image();
        mem.put(0x1100, &[0x90; 0x80]);
        let before = mem.get(0x1000, 0x1000);
        let why = apply_here(&module).unwrap_err();
        assert_eq!(why, NOT_FOUND);
        assert_eq!(mem.get(0x1000, 0x1000), before);
    }

    #[test]
    fn apply_with_the_signature_twice_writes_neither() {
        let (mem, module) = image();
        let code = function_around_site();
        mem.put(0x1100, &code);
        mem.put(0x1800, &code);
        let before = mem.get(0x1000, 0x1000);
        let why = apply_here(&module).unwrap_err();
        assert!(why.starts_with("off: "), "{why}");
        assert!(why.contains("matched 2 times"), "{why}");
        assert_eq!(mem.get(0x1000, 0x1000), before);
    }

    #[test]
    fn apply_on_this_test_binary_finds_no_signature_and_changes_nothing() {
        // The test exe is not the game: its `.text` has no such code. (A match would be written
        // to, so the assertion is also what keeps this test honest.)
        let why = apply_here(&main_module()).unwrap_err();
        assert_eq!(why, NOT_FOUND);
    }

    // ---- the start-up step -------------------------------------------------------------------

    /// `start_up` with an `apply` that records whether it was called.
    fn run(
        cfg: &AsyncCompute,
        cmdline: &str,
        code_patches: bool,
        outcome: Result<(), String>,
    ) -> (Level, String, bool) {
        let called = Cell::new(false);
        let (level, line) = start_up(cfg, cmdline, code_patches, || {
            called.set(true);
            outcome
        });
        (level, line, called.get())
    }

    #[test]
    fn start_up_patches_and_says_on() {
        let (level, line, called) = run(&AsyncCompute::On, "x.exe", true, Ok(()));
        assert!(called);
        assert_eq!(level, Level::Info);
        assert_eq!(line, "async compute: on");
    }

    #[test]
    fn start_up_logs_one_line_per_skip_and_does_not_touch_the_game() {
        for (cfg, cmdline, code_patches, reason) in [
            (AsyncCompute::On, "x.exe", false, "unknown build"),
            (AsyncCompute::Game, "x.exe", true, "ini: game"),
            (
                AsyncCompute::On,
                "x.exe --enable-async-compute",
                true,
                "command line sets it",
            ),
        ] {
            let (level, line, called) = run(&cfg, cmdline, code_patches, Ok(()));
            assert!(!called, "{reason}: apply must not run");
            assert_eq!(level, Level::Info, "{reason}");
            assert_eq!(line, format!("async compute: off: {reason}"));
            assert!(!line.contains('\n'));
        }
    }

    #[test]
    fn start_up_warns_with_the_reason_when_apply_fails() {
        let (level, line, called) = run(
            &AsyncCompute::On,
            "x.exe",
            true,
            Err("off: the code at 0x1 is not the original".to_owned()),
        );
        assert!(called);
        assert_eq!(level, Level::Warn);
        assert_eq!(
            line,
            "async compute: off: the code at 0x1 is not the original"
        );
    }

    // ---- the real game exe (ignored: reads ENSHROUDED_EXE, else the Steam install) -------------

    /// Where the signature matches in build f68140a3 (sha256 b28b448a...; the Steam exe has the
    /// same code at the same address), from the RE notes.
    const MATCH_VA: u64 = 0x1_4084_90d2;

    /// The `.text` section of a PE32+ file: its virtual address (image base included) and the
    /// bytes of it that are in the file. Walks the headers by hand because the parser in `scan`
    /// is private.
    fn text_of(file: &[u8]) -> (u64, &[u8]) {
        use crate::scan::{u16_at, u32_at, u64_at};
        let nt = u32_at(file, 0x3c).unwrap() as usize;
        let n_sections = u16_at(file, nt + 6).unwrap() as usize;
        let optional_size = u16_at(file, nt + 20).unwrap() as usize;
        let optional = nt + 24;
        assert_eq!(u16_at(file, optional), Some(0x20b), "PE32+");
        let image_base = u64_at(file, optional + 24).unwrap();
        let table = optional + optional_size;
        for i in 0..n_sections {
            let at = table + i * 40;
            if file.get(at..at + 8) != Some(b".text\0\0\0".as_slice()) {
                continue;
            }
            let virtual_size = u32_at(file, at + 8).unwrap() as usize;
            let rva = u32_at(file, at + 12).unwrap() as u64;
            let raw_size = u32_at(file, at + 16).unwrap() as usize;
            let raw_ptr = u32_at(file, at + 20).unwrap() as usize;
            let len = virtual_size.min(raw_size);
            return (image_base + rva, &file[raw_ptr..raw_ptr + len]);
        }
        panic!("no .text section");
    }

    #[test]
    #[ignore = "reads the game exe (ENSHROUDED_EXE, else the Steam path); a skip shows only with -- --ignored --nocapture"]
    fn original_bytes_are_in_the_game_exe_and_apply_patches_a_copy_of_its_code() {
        use crate::build_id::test_support::game_exe_or_skip;
        use crate::build_id::{classify, sha256_file, Build};
        use crate::scan::find_unique;
        let Some(path) = game_exe_or_skip("original_bytes_are_in_the_game_exe") else {
            return;
        };
        let build = classify(sha256_file(&path).expect("hash the game exe"));
        assert!(
            matches!(build, Build::Known(_)),
            "this check is for the known build; got {build:?}"
        );
        let file = std::fs::read(&path).expect("read the game exe");
        let (text_va, text) = text_of(&file);

        // The signature matches once in `.text`, where the RE notes say.
        let pattern = Pattern::parse(ASYNC_OVERRIDE.pattern).unwrap();
        let at = find_unique(text, &pattern).expect("the signature is in .text exactly once");
        assert_eq!(text_va + at as u64, MATCH_VA);

        // All 16 original bytes are on disk at +3, the byte the signature skips included, and the
        // instructions around the patch are the ones the module docs show.
        let site = at + ASYNC_OVERRIDE.patch_offset();
        assert_eq!(&text[site..site + 16], &ORIGINAL);
        assert_eq!(&text[at..site], &[0x88, 0x4d, 0x3e], "mov [rbp+0x3e], cl");
        assert_eq!(
            &text[site + 16..site + 25],
            &[0x88, 0x45, 0x3e, 0x88, 0x8d, 0x98, 0x0a, 0x00, 0x00],
            "mov [rbp+0x3e], al; mov [rbp+0xa98], cl"
        );
        // The `je` at +10 is two bytes long and lands on +22: past `mov [rbp+0x3e], al`.
        assert_eq!(text[at + 10], 0x74);
        assert_eq!(10 + 2 + usize::from(text[at + 11]), 22);

        // Run `apply` over a copy of the code around the site in a synthetic image.
        let (mem, module) = image();
        let around = &text[at - 0x80..at + 0x100];
        mem.put(0x1100, around);
        assert_eq!(apply_here(&module), Ok(()));
        let got = mem.get(0x1100, around.len());
        let mut want = around.to_vec();
        want[0x80 + 3..0x80 + 19].copy_from_slice(&NEW);
        assert_eq!(got, want, "only the 16 bytes at +3 changed");
        println!(
            "signature at {MATCH_VA:#x}, patch site {:#x}; apply over a copy of {} bytes of the \
             code changed exactly {} bytes",
            MATCH_VA + 3,
            around.len(),
            got.iter().zip(around).filter(|(a, b)| a != b).count()
        );
    }
}
