//! Headroom scanner API and signature-specific tests.
pub use emberkit::scan::*;
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sigs;
    use emberkit::scan::test_support::synth_pe;
    #[test]
    fn every_sig_parses() {
        for s in sigs::ALL {
            Pattern::parse(s.pattern).unwrap();
        }
    }
    // ---- the real game exe (ignored: reads ENSHROUDED_EXE, else the Steam install) ----------

    /// The anchors from the RE notes: the virtual address each pattern matches at in build
    /// f68140a3 (sha256 b28b448a...; the Steam exe has the same code at the same addresses). The
    /// game-exe test asserts on them, so a pattern that drifts to a different place fails the
    /// test; it is a build-specific check.
    const ANCHORS: [(&str, u64); 12] = [
        ("SCENE_SETTINGS_FN", 0x1_400f_72b0),
        ("SCATTER_BRICK_LOAD", 0x1_4007_9502),
        ("SCATTER_INSTANCE_LOADS", 0x1_4007_691c),
        ("SCATTER_CAP_CONST", 0x1_4007_4d4c),
        ("SCATTER_CAP_ALLOC", 0x1_4007_45d8),
        ("DECO_HIGH_IMM", 0x1_400d_77c3),
        ("DISTANT_HIGH_IMM", 0x1_400d_7816),
        ("VOXEL_HIGH_IMMS", 0x1_400f_7f1f),
        ("ASYNC_OVERRIDE", 0x1_4084_90d2),
        ("HDR_PAPER_WHITE", 0x1_408f_88bc),
        ("TERRAIN_UPDATE_FN", 0x1_401e_2e80),
        ("SCATTER_FRAME_FN", 0x1_4007_67a0),
    ];

    #[test]
    #[ignore = "reads the game exe (ENSHROUDED_EXE, else the Steam path); a skip shows only with -- --ignored --nocapture"]
    fn sigs_unique_in_game_exe() {
        use crate::build_id::test_support::game_exe_or_skip;
        let Some(path) = game_exe_or_skip("sigs_unique_in_game_exe") else {
            return;
        };
        let file = std::fs::read(&path).expect("read the game exe");
        let info = parse_pe(&file).expect("PE headers of the game exe");
        let mut failures = Vec::new();
        let mut drift = Vec::new();
        for sig in sigs::ALL {
            let p = Pattern::parse(sig.pattern).unwrap();
            // Map each named section by file offset: the part of it that is both in the file and
            // in memory (the lesser of raw size and virtual size).
            let mut regions: Vec<(usize, &[u8])> = Vec::new();
            for name in sig.sections {
                for s in info.sections.iter().filter(|s| s.is_named(name)) {
                    let (rva, len) = s
                        .mem_range(info.size_of_image as usize)
                        .expect("section in image");
                    let raw = len.min(s.raw_size as usize);
                    let start = s.raw_ptr as usize;
                    let bytes = file.get(start..start + raw).expect("section bytes in file");
                    regions.push((info.image_base as usize + rva, bytes));
                }
            }
            assert!(
                !regions.is_empty(),
                "{}: sections {:?} not in the exe",
                sig.name,
                sig.sections
            );
            let anchor = ANCHORS
                .iter()
                .find(|(n, _)| *n == sig.name)
                .map(|(_, a)| *a)
                .expect("anchor");
            match find_unique_in_regions(&regions, &p) {
                Ok(va) => {
                    let sites: Vec<String> = sig
                        .patch_offsets
                        .iter()
                        .map(|o| format!("{:#x}", va + o))
                        .collect();
                    let delta = va as i64 - anchor as i64;
                    let note = if delta == 0 {
                        "= anchor".to_string()
                    } else {
                        format!("anchor {anchor:#x}, delta {delta:+} bytes")
                    };
                    println!(
                        "{:<24} match {va:#x}  patch sites [{}]  ({note})",
                        sig.name,
                        sites.join(", ")
                    );
                    if delta != 0 {
                        drift.push(format!(
                            "{}: matched {va:#x}, anchor {anchor:#x}, delta {delta:+} bytes",
                            sig.name
                        ));
                    }
                }
                Err(e) => failures.push(format!("{}: {e:?}", sig.name)),
            }
        }
        assert!(failures.is_empty(), "not exactly once: {failures:?}");
        assert!(drift.is_empty(), "match differs from its anchor: {drift:?}");
    }

    /// Independent file-only oracles: no game executable is loaded as code or written.
    mod file_oracles {
        use super::*;
        use iced_x86::{
            ConstantOffsets, Decoder, DecoderOptions, Formatter, Instruction, IntelFormatter,
            Mnemonic, Register,
        };

        /// Check a local offset by decoding from a verified instruction start, not by recovering CFG.
        fn instruction_boundary(bytes: &[u8], ip: u64, offset: usize) -> Result<(), String> {
            if offset >= bytes.len() {
                return Err(format!("offset +{offset:#x} is outside code"));
            }
            let mut decoder = Decoder::with_ip(64, bytes, ip, DecoderOptions::NONE);
            while decoder.position() < offset {
                let start = decoder.position();
                let instruction = decoder.decode();
                if instruction.is_invalid() {
                    return Err(format!(
                        "invalid instruction at +{start:#x}: {}",
                        intel(&instruction)
                    ));
                }
                if decoder.position() > offset {
                    return Err(format!(
                        "offset +{offset:#x} is inside +{start:#x}: {}",
                        intel(&instruction)
                    ));
                }
            }
            Ok(())
        }

        /// Intel syntax is included in every instruction-layout diagnostic.
        fn intel(instruction: &Instruction) -> String {
            let mut text = String::new();
            IntelFormatter::new().format(instruction, &mut text);
            text
        }

        /// Validate the documented patch contract using independent decoder constant offsets.
        fn validate_sig(sig: &sigs::Sig, bytes: &[u8], ip: u64) -> Result<Vec<String>, String> {
            let length = Pattern::parse(sig.pattern)
                .map_err(|error| error.to_string())?
                .len();
            let rows = decode_prefix(bytes, ip, length)?;
            let first = rows.first().ok_or("empty instruction stream")?;
            let first_mnemonic = match sig.name {
                "SCATTER_BRICK_LOAD"
                | "SCATTER_INSTANCE_LOADS"
                | "DECO_HIGH_IMM"
                | "DISTANT_HIGH_IMM" => Mnemonic::Cmp,
                "SCENE_SETTINGS_FN" | "TERRAIN_UPDATE_FN" | "SCATTER_FRAME_FN"
                | "SCATTER_CAP_CONST" | "SCATTER_CAP_ALLOC" | "VOXEL_HIGH_IMMS"
                | "ASYNC_OVERRIDE" | "HDR_PAPER_WHITE" => Mnemonic::Mov,
                name => return Err(format!("no documented oracle contract for {name}")),
            };
            if first.start != 0
                || first.instruction.ip() != ip
                || first.instruction.mnemonic() != first_mnemonic
            {
                return Err(format!(
                    "{}: unexpected verified signature start: {}",
                    sig.name,
                    first.summary()
                ));
            }
            if matches!(
                sig.name,
                "SCENE_SETTINGS_FN" | "TERRAIN_UPDATE_FN" | "SCATTER_FRAME_FN"
            ) {
                if sig.patch_offsets != [0] {
                    return Err(format!(
                        "{}: hook must start at its verified entry, got {:?}\n{}",
                        sig.name,
                        sig.patch_offsets,
                        first.summary()
                    ));
                }
                instruction_boundary(bytes, ip, sig.patch_offset())?;
                let mut covered = 0;
                let mut lengths = Vec::new();
                for row in &rows {
                    if covered >= 5 {
                        break;
                    }
                    lengths.push(row.instruction.len());
                    covered += row.instruction.len();
                }
                let expected: &[usize] = if sig.name == "SCENE_SETTINGS_FN" {
                    &[3, 8]
                } else {
                    &[5]
                };
                if lengths != expected {
                    return Err(format!(
                        "{}: hook coverage {:?}, expected {:?}\n{}",
                        sig.name,
                        lengths,
                        expected,
                        rows.iter()
                            .map(Decoded::summary)
                            .collect::<Vec<_>>()
                            .join("\n")
                    ));
                }
                if sig.name == "SCENE_SETTINGS_FN" {
                    if first.instruction.op0_register() != Register::EAX
                        || first.instruction.memory_base() != Register::R8
                        || rows[1].instruction.mnemonic() != Mnemonic::Movss
                        || rows[1].instruction.memory_base() != Register::RIP
                    {
                        return Err(format!(
                            "{}: expected mov eax,[r8] then RIP-relative movss",
                            sig.name
                        ));
                    }
                } else {
                    let source = if sig.name == "TERRAIN_UPDATE_FN" {
                        Register::RBX
                    } else {
                        Register::RDX
                    };
                    if first.instruction.memory_base() != Register::RSP
                        || first.instruction.memory_displacement64() != 0x10
                        || first.instruction.op1_register() != source
                    {
                        return Err(format!(
                            "{}: wrong hook prologue: {}",
                            sig.name,
                            first.summary()
                        ));
                    }
                }
                return Ok(rows.iter().map(Decoded::summary).collect());
            }
            if sig.name == "ASYNC_OVERRIDE" {
                if sig.patch_offsets.len() != 1 {
                    return Err("ASYNC_OVERRIDE requires one 16-byte span".into());
                }
                let start = sig.patch_offset();
                let end = start.checked_add(16).ok_or("async span overflow")?;
                instruction_boundary(bytes, ip, start)?;
                let span: Vec<_> = rows
                    .iter()
                    .filter(|row| (start..end).contains(&row.start))
                    .collect();
                let lengths: Vec<_> = span.iter().map(|row| row.instruction.len()).collect();
                let mnemonics: Vec<_> = span.iter().map(|row| row.instruction.mnemonic()).collect();
                if span.first().map(|row| row.start) != Some(start)
                    || span.last().map(|row| row.start + row.instruction.len()) != Some(end)
                    || lengths != [7, 2, 7]
                    || mnemonics != [Mnemonic::Cmp, Mnemonic::Je, Mnemonic::Movzx]
                {
                    return Err(format!("ASYNC_OVERRIDE: +{start:#x}..+{end:#x} must be whole cmp/je/movzx instructions [7,2,7]\n{}",rows.iter().map(Decoded::summary).collect::<Vec<_>>().join("\n")));
                }
                // The existing module disassembly documents these three fields inside +3..+19.
                exact_field(sig.name, &rows, 6, 4, Field::Displacement(Mnemonic::Cmp))?;
                exact_field(sig.name, &rows, 11, 1, Field::Branch)?;
                exact_field(sig.name, &rows, 15, 4, Field::Displacement(Mnemonic::Movzx))?;
                if span[0].instruction.memory_base() != Register::RSI
                    || span[0].instruction.memory_displacement64() != 0x35289
                    || span[2].instruction.memory_base() != Register::RSI
                    || span[2].instruction.memory_displacement64() != 0x35288
                {
                    return Err("ASYNC_OVERRIDE: unexpected optional-bool addresses in decoded instructions".into());
                }
                return Ok(rows.iter().map(Decoded::summary).collect());
            }
            let fields = match sig.name {
                "SCATTER_BRICK_LOAD" => vec![Field::RipLoad(Register::XMM0)],
                "SCATTER_INSTANCE_LOADS" => vec![Field::RipLoad(Register::XMM8)],
                "SCATTER_CAP_CONST" | "SCATTER_CAP_ALLOC" => vec![Field::Immediate(0x14000)],
                "DECO_HIGH_IMM" => vec![Field::Immediate(1.5f32.to_bits())],
                "DISTANT_HIGH_IMM" => vec![Field::Immediate(2.5f32.to_bits())],
                "VOXEL_HIGH_IMMS" => vec![
                    Field::Immediate(32.0f32.to_bits()),
                    Field::Immediate(128.0f32.to_bits()),
                    Field::Immediate(160.0f32.to_bits()),
                ],
                "HDR_PAPER_WHITE" => vec![Field::Immediate(250.0f32.to_bits())],
                name => return Err(format!("oracle layout not implemented for {name}")),
            };
            if sig.patch_offsets.len() != fields.len() {
                return Err(format!(
                    "{}: expected {} documented fields, got {:?}",
                    sig.name,
                    fields.len(),
                    sig.patch_offsets
                ));
            }
            for (&offset, kind) in sig.patch_offsets.iter().zip(fields) {
                exact_field(sig.name, &rows, offset, 4, kind)?;
            }
            Ok(rows.iter().map(Decoded::summary).collect())
        }

        /// One instruction decoded from a documented signature start.
        struct Decoded {
            /// Byte offset from the documented signature start.
            start: usize,
            /// The independently decoded 64-bit instruction.
            instruction: Instruction,
            /// Constant fields reported by iced, relative to that instruction.
            constants: ConstantOffsets,
        }

        impl Decoded {
            /// Include Intel syntax and both independently decoded constant ranges.
            fn summary(&self) -> String {
                format!(
                    "{:#x} +{:#x} len {}: {} (imm +{:#x}/{}; disp +{:#x}/{})",
                    self.instruction.ip(),
                    self.start,
                    self.instruction.len(),
                    intel(&self.instruction),
                    self.start + self.constants.immediate_offset(),
                    self.constants.immediate_size(),
                    self.start + self.constants.displacement_offset(),
                    self.constants.displacement_size()
                )
            }
        }

        /// Decode whole instructions covering a signature; no control-flow recovery is claimed.
        fn decode_prefix(bytes: &[u8], ip: u64, minimum: usize) -> Result<Vec<Decoded>, String> {
            let mut decoder = Decoder::with_ip(64, bytes, ip, DecoderOptions::NONE);
            let mut rows = Vec::new();
            while decoder.position() < minimum {
                if !decoder.can_decode() {
                    return Err(format!("truncated code before +{minimum:#x}"));
                }
                let start = decoder.position();
                let instruction = decoder.decode();
                if instruction.is_invalid() {
                    return Err(format!(
                        "invalid instruction at {:#x}: {}",
                        instruction.ip(),
                        intel(&instruction)
                    ));
                }
                let constants = decoder.get_constant_offsets(&instruction);
                rows.push(Decoded {
                    start,
                    instruction,
                    constants,
                });
            }
            Ok(rows)
        }

        /// The exact constant kind and, for documented immediates, original bits.
        #[derive(Debug, Clone, Copy)]
        enum Field {
            /// A mov imm32 with the documented original bits.
            Immediate(u32),
            /// A memory displacement belonging to the documented instruction kind.
            Displacement(Mnemonic),
            /// The specific RIP-relative scatter distance load, not another nearby load.
            RipLoad(Register),
            /// The async span's one-byte conditional branch field.
            Branch,
        }

        /// A patch field must equal one complete iced constant range, never a byte inside it.
        fn exact_field(
            name: &str,
            rows: &[Decoded],
            offset: usize,
            size: usize,
            kind: Field,
        ) -> Result<(), String> {
            let end = offset.checked_add(size).ok_or("patch range overflow")?;
            let found = rows.iter().find(|row| {
                let (field, length) = match kind {
                    Field::Immediate(_) | Field::Branch => (
                        row.constants.immediate_offset(),
                        row.constants.immediate_size(),
                    ),
                    Field::Displacement(_) | Field::RipLoad(_) => (
                        row.constants.displacement_offset(),
                        row.constants.displacement_size(),
                    ),
                };
                length == size && length != 0 && row.start + field == offset
            });
            let context = || {
                rows.iter()
                    .map(Decoded::summary)
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let row = found.ok_or_else(|| {
                format!(
                    "{name}: +{offset:#x}..+{end:#x} is not one exact {kind:?} field\n{}",
                    context()
                )
            })?;
            let expected = match kind {
                Field::Immediate(value) => {
                    row.instruction.mnemonic() == Mnemonic::Mov
                        && row.instruction.immediate32() == value
                }
                Field::Displacement(mnemonic) => row.instruction.mnemonic() == mnemonic,
                Field::RipLoad(register) => {
                    row.instruction.mnemonic() == Mnemonic::Movss
                        && row.instruction.memory_base() == Register::RIP
                        && row.instruction.op0_register() == register
                }
                Field::Branch => row.instruction.mnemonic() == Mnemonic::Je,
            };
            if !expected {
                return Err(format!(
                    "{name}: wrong instruction/value for +{offset:#x} {kind:?}: {}",
                    row.summary()
                ));
            }
            Ok(())
        }

        /// Fill fixture wildcards with zero; callers supply any documented immediate values.
        fn signature_bytes(sig: &sigs::Sig) -> Vec<u8> {
            sig.pattern
                .split_ascii_whitespace()
                .map(|byte| u8::from_str_radix(byte, 16).unwrap_or(0))
                .collect()
        }

        /// Check all three hook contracts and reject shifted/truncated prologues.
        #[test]
        fn all_documented_hook_entries_cover_whole_instructions() {
            for sig in [
                &sigs::SCENE_SETTINGS_FN,
                &sigs::TERRAIN_UPDATE_FN,
                &sigs::SCATTER_FRAME_FN,
            ] {
                let bytes = signature_bytes(sig);
                assert!(validate_sig(sig, &bytes, 0).is_ok(), "{}", sig.name);
                let wrong = sigs::Sig {
                    patch_offsets: &[1],
                    ..*sig
                };
                assert!(validate_sig(&wrong, &bytes, 0).is_err(), "{}", sig.name);
                assert!(validate_sig(sig, &bytes[..4], 0).is_err(), "{}", sig.name);
            }
        }

        /// The documented async patch spans cmp/je/movzx rather than one constant field.
        #[test]
        fn async_patch_is_three_whole_instructions_not_one_constant() {
            let mut bytes = signature_bytes(&sigs::ASYNC_OVERRIDE);
            assert!(validate_sig(&sigs::ASYNC_OVERRIDE, &bytes, 0).is_ok());
            let shifted = sigs::Sig {
                patch_offsets: &[4],
                ..sigs::ASYNC_OVERRIDE
            };
            assert!(validate_sig(&shifted, &bytes, 0).is_err());
            bytes[12] = 0x90;
            assert!(validate_sig(&sigs::ASYNC_OVERRIDE, &bytes, 0).is_err());
        }

        /// Raw PE header values; VirtualSize and SizeOfRawData stay separate.
        #[derive(Debug, PartialEq, Eq)]
        struct RawSection {
            /// The exact eight header bytes, including NUL padding.
            name: [u8; 8],
            /// VirtualAddress is an RVA in IMAGE_SECTION_HEADER.
            virtual_address: u32,
            /// Unaligned virtual length, without substituting a raw size.
            virtual_size: u32,
            /// PointerToRawData in the file.
            raw_offset: u32,
            /// SizeOfRawData, independently of VirtualSize.
            raw_size: u32,
        }

        /// Compare actual IMAGE_SECTION_HEADER fields, not unified section-size semantics.
        fn compare_pe_sections(bytes: &[u8], ours: &PeInfo) -> Result<Vec<String>, String> {
            use object::LittleEndian as LE;
            let independent = object::read::pe::PeFile64::parse(bytes)
                .map_err(|error| format!("object PE parse: {error}"))?;
            let table = independent.section_table();
            if table.len() != ours.sections.len() {
                return Err(format!(
                    "section count: ours {}, object {}",
                    ours.sections.len(),
                    table.len()
                ));
            }
            let image_base = independent.nt_headers().optional_header.image_base.get(LE);
            if image_base != ours.image_base {
                return Err(format!(
                    "image base: ours {:#x}, object {image_base:#x}",
                    ours.image_base
                ));
            }
            let mut summaries = Vec::new();
            for (index, (section, raw)) in ours.sections.iter().zip(table.iter()).enumerate() {
                let actual = RawSection {
                    name: section.name,
                    virtual_address: section.rva,
                    virtual_size: section.virtual_size,
                    raw_offset: section.raw_ptr,
                    raw_size: section.raw_size,
                };
                let expected = RawSection {
                    name: raw.name,
                    virtual_address: raw.virtual_address.get(LE),
                    virtual_size: raw.virtual_size.get(LE),
                    raw_offset: raw.pointer_to_raw_data.get(LE),
                    raw_size: raw.size_of_raw_data.get(LE),
                };
                if actual != expected {
                    return Err(format!("section {index}: raw header mismatch\nours {actual:#x?}\nobject {expected:#x?}"));
                }
                summaries.push(format!("section {index} {}: RVA={:#x} VA={:#x} virtual_size={:#x} raw_offset={:#x} raw_size={:#x}",section.name_text(),actual.virtual_address,image_base+u64::from(actual.virtual_address),actual.virtual_size,actual.raw_offset,actual.raw_size));
            }
            Ok(summaries)
        }

        /// Alter each raw field in each synthetic section to make missing comparisons visible.
        #[test]
        fn pe_oracle_distinguishes_virtual_and_raw_fields_and_checks_every_row() {
            let bytes = synth_pe(
                &[
                    (".text", 0x1234, 0x1000, 0x200, 0x400),
                    (".rdata", 0x111, 0x3000, 0x400, 0x600),
                ],
                0x5000,
            );
            assert!(compare_pe_sections(&bytes, &parse_pe_file(&bytes).unwrap()).is_ok());
            for index in 0..2 {
                for changed in 0..5 {
                    let mut ours = parse_pe_file(&bytes).unwrap();
                    let row = &mut ours.sections[index];
                    match changed {
                        0 => row.name[0] = b'!',
                        1 => row.rva += 1,
                        2 => row.virtual_size = row.raw_size,
                        3 => row.raw_ptr += 1,
                        4 => row.raw_size = row.virtual_size,
                        _ => unreachable!(),
                    }
                    assert!(
                        compare_pe_sections(&bytes, &ours).is_err(),
                        "section {index}, changed field {changed}"
                    );
                }
            }
            let mut missing = parse_pe_file(&bytes).unwrap();
            missing.sections.pop();
            assert!(compare_pe_sections(&bytes, &missing).is_err());
        }

        /// Preserve the existing Steam helper's explicit-path panic/default-missing skip rules.
        fn steam_image(test: &str) -> Option<(Vec<u8>, PeInfo)> {
            use crate::build_id::test_support::steam_exe_or_skip;
            use crate::build_id::{classify, sha256_file, to_hex};
            let path = steam_exe_or_skip(test)?;
            let hash = sha256_file(&path).expect("hash Steam exe");
            println!(
                "file {} SHA256={} {:?}",
                path.display(),
                to_hex(&hash),
                classify(hash)
            );
            let bytes = std::fs::read(&path).expect("read Steam exe as data");
            let pe = parse_pe_file(&bytes).expect("shared file PE parser");
            Some((bytes, pe))
        }

        /// Decode layouts at RE-verified signature starts; this does not recover control flow.
        #[test]
        #[ignore = "reads Steam exe only (ENSHROUDED_STEAM_EXE, else Steam path); run by exact name with --ignored --nocapture"]
        fn instruction_fields_match_steam_exe() {
            let Some((bytes, pe)) = steam_image("instruction_fields_match_steam_exe") else {
                return;
            };
            let (text_va, text) = pe
                .section_va_bytes(&bytes, ".text")
                .expect("file .text bytes");
            let mut failures = Vec::new();
            for sig in sigs::ALL {
                assert_eq!(
                    sig.sections,
                    [".text"],
                    "{} requires an explicit section policy",
                    sig.name
                );
                let pattern = Pattern::parse(sig.pattern).expect("signature pattern");
                let at = match find_unique(text, &pattern) {
                    Ok(at) => at,
                    Err(error) => {
                        failures.push(format!("{}: {error}", sig.name));
                        continue;
                    }
                };
                let ip = text_va + at as u64;
                let rest = &text[at..];
                let window = &rest[..pattern.len().max(32).min(rest.len())];
                println!(
                    "{} match {ip:#x}, patch offsets {:?}",
                    sig.name, sig.patch_offsets
                );
                // The addresses are the existing, documented anchors, not a global linear sweep.
                match ANCHORS.iter().find(|(name, _)| *name == sig.name) {
                    Some((_, anchor)) if *anchor == ip => (),
                    other => failures.push(format!(
                        "{}: start {ip:#x} differs from verified anchor {other:?}",
                        sig.name
                    )),
                }
                match validate_sig(sig, window, ip) {
                    Ok(lines) => {
                        for line in lines {
                            println!("  {line}");
                        }
                    }
                    Err(error) => {
                        let decoded = decode_prefix(window, ip, pattern.len())
                            .map(|rows| {
                                rows.iter()
                                    .map(Decoded::summary)
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .unwrap_or_else(|error| error);
                        failures.push(format!(
                            "{} at {ip:#x}: {error}\ndecoded instructions:\n{decoded}",
                            sig.name
                        ));
                    }
                }
            }
            assert!(
                failures.is_empty(),
                "instruction oracle failures:\n{}",
                failures.join("\n")
            );
        }

        /// Compare every raw PE section header against object's independent parser.
        #[test]
        #[ignore = "reads Steam exe only (ENSHROUDED_STEAM_EXE, else Steam path); run by exact name with --ignored --nocapture"]
        fn pe_section_headers_match_steam_exe() {
            let Some((bytes, pe)) = steam_image("pe_section_headers_match_steam_exe") else {
                return;
            };
            let lines = compare_pe_sections(&bytes, &pe).unwrap_or_else(|error| panic!("{error}"));
            for line in lines {
                println!("{line}");
            }
        }

        /// The distance is the third load; another valid RIP displacement is not interchangeable.
        #[test]
        fn instance_oracle_rejects_another_valid_rip_displacement() {
            let bytes = signature_bytes(&sigs::SCATTER_INSTANCE_LOADS);
            assert!(validate_sig(&sigs::SCATTER_INSTANCE_LOADS, &bytes, 0).is_ok());
            let wrong = sigs::Sig {
                patch_offsets: &[13],
                ..sigs::SCATTER_INSTANCE_LOADS
            };
            let error = validate_sig(&wrong, &bytes, 0).unwrap_err();
            assert!(error.contains("movss"), "{error}");
        }

        /// Reject shifted imm32 and disp32 fields while accepting their exact documented ranges.
        #[test]
        fn a_shifted_patch_must_fail_the_exact_constant_field_check() {
            let mut bytes = signature_bytes(&sigs::HDR_PAPER_WHITE);
            bytes[3..7].copy_from_slice(&250.0f32.to_le_bytes());
            assert!(validate_sig(&sigs::HDR_PAPER_WHITE, &bytes, 0).is_ok());
            let wrong = sigs::Sig {
                patch_offsets: &[4],
                ..sigs::HDR_PAPER_WHITE
            };
            let error = validate_sig(&wrong, &bytes, 0).unwrap_err();
            assert!(error.contains("mov"), "{error}");
            assert!(error.contains("+0x4"), "{error}");
            let bytes = signature_bytes(&sigs::SCATTER_BRICK_LOAD);
            assert!(validate_sig(&sigs::SCATTER_BRICK_LOAD, &bytes, 0).is_ok());
            let wrong = sigs::Sig {
                patch_offsets: &[10],
                ..sigs::SCATTER_BRICK_LOAD
            };
            assert!(validate_sig(&wrong, &bytes, 0).is_err());
        }

        /// A synthetic stream proves that decoding from an arbitrary byte is insufficient.
        #[test]
        fn a_match_inside_an_immediate_is_not_an_instruction_boundary() {
            // Fixture only: mov eax,90909090h; ret. Starting at +2 would decode a false nop.
            let bytes = [0xc7, 0xc0, 0x90, 0x90, 0x90, 0x90, 0xc3];
            assert!(instruction_boundary(&bytes, 0, 0).is_ok());
            assert!(instruction_boundary(&bytes, 0, 6).is_ok());
            let error = instruction_boundary(&bytes, 0, 2).unwrap_err();
            assert!(error.contains("mov"), "{error}");
            assert!(instruction_boundary(&bytes, 0, bytes.len()).is_err());
        }
    }
}
