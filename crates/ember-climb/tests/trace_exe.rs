//! File-only oracles for the optional Jump observers. No game code or copied code is executed.
//!
//! All four tests are ignored by default and require their explicitly named environment variable.
//! A missing file or wrong exact SHA256 fails; no image can silently substitute for another.
//! The shared emberkit file locator, hash classifier and checked PE reader own file handling.

use std::path::Path;

use ember_climb::trace_sigs::{self, Layout, Targets};
use emberkit::build_id::{self, test_support, Build};
use emberkit::scan::{find_unique, parse_pe_file, Pattern, PeInfo};
use iced_x86::{Decoder, DecoderOptions, OpKind};

/// One exact owned image receipt, with native targets in accepted/accessor/dispatcher order.
struct Expected {
    /// Required explicit path variable.
    env: &'static str,
    /// Exact full-file identity from the public supported-build table.
    sha256: &'static str,
    /// Exact known build label.
    label: &'static str,
    /// Image-relative native function entries.
    rvas: [usize; 3],
    /// Admitted native entity table layout.
    layout: Layout,
}

/// One checked file snapshot, never mapped as executable memory.
struct Image {
    /// Complete owned file bytes.
    bytes: Vec<u8>,
    /// Shared checked PE metadata.
    pe: PeInfo,
    /// Identity verified against the full-file SHA256.
    label: &'static str,
}

impl Image {
    /// Read only the exact explicit file and reuse emberkit's path/hash/PE helpers.
    fn open(expected: &Expected) -> Self {
        let explicit = std::env::var_os(expected.env)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| panic!("set {} to this oracle's exact pinned image", expected.env));
        let path = test_support::locate(
            "Jump observation file oracle",
            expected.env,
            Some(explicit),
            Path::new(test_support::STEAM_EXE),
        )
        .expect("an ignored file oracle must fail when its image is unavailable");
        let hash = build_id::sha256_file(&path).unwrap();
        assert_eq!(
            build_id::to_hex(&hash),
            expected.sha256,
            "wrong exact image for {}",
            expected.env
        );
        assert_eq!(build_id::classify(hash), Build::Known(expected.label));
        let bytes = std::fs::read(path).unwrap();
        let pe = parse_pe_file(&bytes).unwrap();
        Self {
            bytes,
            pe,
            label: expected.label,
        }
    }

    /// Snapshot checked file bytes at a virtual address.
    fn read(&self, at: usize, len: usize) -> Option<Vec<u8>> {
        self.pe
            .bytes_at_va(&self.bytes, u64::try_from(at).ok()?, len)
            .ok()
            .map(<[u8]>::to_vec)
    }

    /// Run the same whole-set startup preflight with the shared unique `.text` scanner.
    fn preflight(&self) -> Result<Targets, String> {
        let (base, text) = self.pe.section_va_bytes(&self.bytes, ".text")?;
        trace_sigs::preflight(
            self.label,
            |sig| {
                let pattern = Pattern::parse(sig.pattern).map_err(|why| why.to_string())?;
                let offset = find_unique(text, &pattern).map_err(|why| why.to_string())?;
                let address = usize::try_from(base)
                    .ok()
                    .and_then(|base| base.checked_add(offset))
                    .ok_or("oracle address overflow")?;
                Ok(vec![address])
            },
            |at, len| self.read(at, len),
        )
    }

    /// Check the original unwind row/prologue without claiming generated-trampoline unwind data.
    fn original_prologue(&self, at: usize, expected_size: u8) {
        let (_, pdata) = self.pe.section_va_bytes(&self.bytes, ".pdata").unwrap();
        let row = pdata
            .as_chunks::<12>()
            .0
            .iter()
            .find(|row| {
                let begin = u32::from_le_bytes(row[0..4].try_into().unwrap());
                self.pe.image_base + u64::from(begin) == at as u64
            })
            .expect("native observer target needs its original unwind entry");
        let unwind =
            self.pe.image_base + u64::from(u32::from_le_bytes(row[8..12].try_into().unwrap()));
        let header = self.read(unwind as usize, 4).unwrap();
        assert_eq!(header[0] & 7, 1, "native unwind version");
        assert_eq!(
            header[1], expected_size,
            "native prologue length at {at:#x}"
        );
    }

    /// Decode the exact instructions MinHook initially copies, without executing any bytes.
    fn initial_copy(&self, at: usize, expected_length: usize) {
        let bytes = self.read(at, 32).unwrap();
        let mut decoder = Decoder::with_ip(64, &bytes, at as u64, DecoderOptions::NONE);
        let mut length = 0;
        while length < 5 {
            let instruction = decoder.decode();
            assert!(!instruction.is_invalid());
            assert!(!instruction.is_ip_rel_memory_operand());
            assert!(!(0..instruction.op_count()).any(|operand| matches!(
                instruction.op_kind(operand),
                OpKind::NearBranch16
                    | OpKind::NearBranch32
                    | OpKind::NearBranch64
                    | OpKind::FarBranch16
                    | OpKind::FarBranch32
            )));
            length += instruction.len();
        }
        assert_eq!(length, expected_length, "native initial copy at {at:#x}");
    }
}

/// All three entries and their native provenance must agree in each exact supported image.
fn oracle(expected: Expected) {
    let image = Image::open(&expected);
    let targets = image.preflight().unwrap();
    assert_eq!(targets.layout, expected.layout);
    for ((at, rva), (copy_size, prologue_size)) in
        [targets.accepted, targets.accessor, targets.dispatcher]
            .into_iter()
            .zip(expected.rvas)
            .zip([(5, 32), (6, 6), (7, 101)])
    {
        assert_eq!(at as u64, image.pe.image_base + rva as u64);
        image.initial_copy(at, copy_size);
        image.original_prologue(at, prologue_size);
    }
}

/// The previous owned client image pins both native call-follow sites and the old identity layout.
#[test]
#[ignore = "requires ENSHROUDED_PREVIOUS_STEAM_EXE; file bytes only"]
fn previous_client_jump_observer_contract() {
    oracle(Expected {
        env: "ENSHROUDED_PREVIOUS_STEAM_EXE",
        sha256: "b28b448a8dbf48c932b717f035031f5a89712a7cccfcc2d6cac0d47e6421c12a",
        label: "f68140a3",
        rvas: [0x2c3f90, 0x213220, 0x41a5c0],
        layout: Layout::Previous,
    });
}

/// The current owned client image pins both native call-follow sites and the current identity layout.
#[test]
#[ignore = "requires ENSHROUDED_STEAM_EXE; file bytes only"]
fn current_client_jump_observer_contract() {
    oracle(Expected {
        env: "ENSHROUDED_STEAM_EXE",
        sha256: "0403981fd2e8ad18bd8c5698dff03c8d505b5a46677832c496db9c645513f2a6",
        label: "1283531-steam",
        rvas: [0x2c8f60, 0x216b70, 0x421510],
        layout: Layout::Current,
    });
}

/// The previous owned server image pins the same borrowed-input contract at independent RVAs.
#[test]
#[ignore = "requires ENSHROUDED_PREVIOUS_SERVER_EXE; file bytes only"]
fn previous_server_jump_observer_contract() {
    oracle(Expected {
        env: "ENSHROUDED_PREVIOUS_SERVER_EXE",
        sha256: "d7baa768eb00aa895d9f5a1cd8747ccf66858c7b542f50983092b2e3261588ee",
        label: "d7baa768-server",
        rvas: [0xb08a0, 0x30a00, 0x1bb510],
        layout: Layout::Previous,
    });
}

/// The current owned server image pins the same borrowed-input contract at independent RVAs.
#[test]
#[ignore = "requires ENSHROUDED_SERVER_EXE; file bytes only"]
fn current_server_jump_observer_contract() {
    oracle(Expected {
        env: "ENSHROUDED_SERVER_EXE",
        sha256: "4429653ac0cfa154bc08e138853caa5d8557f82129b7488031f40345816aa6dc",
        label: "1283531-server",
        rvas: [0xb4590, 0x33ba0, 0x1c0dd0],
        layout: Layout::Current,
    });
}
