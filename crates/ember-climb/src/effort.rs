//! A minimum effort in the native wall-climb stamina calculation.
//!
//! The exact native span and live-register contract are in docs/re-notes.md, W52 hanging stamina.
//! This is an inline calculation: it keeps the game's accumulator, attributes and exhaustion path.

/// How far the calculation begins before the existing stamina cost-call signature.
pub const BEFORE_COST: usize = 0x1d5;
/// The bytes owned by the calculation, ending at the native debit body.
pub const SIZE: usize = 82;
/// The native distance scaling and motion-only early-out, identical in all four supported images.
pub const GAME: [u8; SIZE] = [
    0xf3, 0x44, 0x0f, 0x59, 0xd2, 0xf3, 0x0f, 0x59, 0xf2, 0x41, 0x0f, 0x28, 0xc2, 0xf3, 0x44, 0x0f,
    0x59, 0xca, 0x41, 0x0f, 0x57, 0xc3, 0x41, 0x0f, 0x28, 0xca, 0xf3, 0x0f, 0x5f, 0xc8, 0x44, 0x0f,
    0x2f, 0xe1, 0x72, 0x2e, 0x0f, 0x28, 0xc6, 0x0f, 0x28, 0xce, 0x41, 0x0f, 0x57, 0xc3, 0xf3, 0x0f,
    0x5f, 0xc8, 0x44, 0x0f, 0x2f, 0xe1, 0x72, 0x1a, 0x41, 0x0f, 0x28, 0xc1, 0x41, 0x0f, 0x28, 0xc9,
    0x41, 0x0f, 0x57, 0xc3, 0xf3, 0x0f, 0x5f, 0xc8, 0x44, 0x0f, 0x2f, 0xe1, 0x0f, 0x83, 0x23, 0x02,
    0x00, 0x00,
];

/// The owned inline calculation. Only the local distance changes below the half-metre/second floor.
pub const HOLD: [u8; SIZE] = [
    // Scale the three native local components by the unchanged native dt.
    0xf3, 0x44, 0x0f, 0x59, 0xd2, 0xf3, 0x0f, 0x59, 0xf2, 0xf3, 0x44, 0x0f, 0x59, 0xca,
    // Pack y,x,z; DPPS0x71 sums (y*y+x*x)+(z*z+0), preserving the native scalar order.
    0x0f, 0x28, 0xc6, 0x41, 0x0f, 0x14, 0xc2, 0x41, 0x0f, 0x16, 0xc1, 0x66, 0x0f, 0x3a, 0x40, 0xc0,
    0x71,
    // EAX is dead. Zero dt jumps straight to JAE with CF=0 from TEST; nothing is charged.
    0x66, 0x0f, 0x7e, 0xd0, 0x85, 0xc0, 0x74, 0x16,
    // Every nonzero native dt is a finite normal float. Decrement its exponent to halve it.
    0x2d, 0x00, 0x00, 0x80, 0x00, 0x66, 0x0f, 0x6e, 0xc8, 0xf3, 0x0f, 0x59, 0xc9,
    // Keep original locals when actual squared distance exceeds the floor, or is unordered.
    0x0f, 0x2f, 0xc8, 0x72, 0x19,
    // Zero squared floor skips to the original next entity. Its relocated displacement is0x232.
    0x44, 0x0f, 0x2f, 0xe1, 0x0f, 0x83, 0x32, 0x02, 0x00, 0x00,
    // Otherwise give the native debit a distance of abs(dt)*0.5. These are locals, not Velocity.
    0xf3, 0x44, 0x0f, 0x51, 0xd1, 0x0f, 0x57, 0xf6, 0x45, 0x0f, 0x57, 0xc9, 0x90, 0x90, 0x90,
];

/// Refuse startup before any patch when this CPU cannot execute the owned DPPS instruction.
pub fn require_cpu(sse41: bool) -> Result<(), String> {
    if sse41 {
        Ok(())
    } else {
        Err("climbing needs SSE4.1 for its stamina calculation: nothing patched".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emberkit::patch::test_support::Mem;
    use iced_x86::{Decoder, DecoderOptions};

    /// A private executable wrapper and its complete backing allocation.
    struct Harness {
        /// The initialized executable pages; no game code or process memory is used.
        memory: Mem,
    }

    impl Harness {
        /// Wrap one calculation with native-register inputs and inert branch destinations.
        fn new(calculation: &[u8; SIZE]) -> Harness {
            assert!(
                std::is_x86_feature_detected!("sse4.1"),
                "native-code fixture needs SSE4.1"
            );
            // Save the native nonvolatile registers this fixture supplies. Entry RSP is 8 mod16;
            // subtracting0x78 aligns the five16-byte save slots. The calculation changes no stack.
            let mut code = vec![
                0x48, 0x83, 0xec, 0x78, 0x0f, 0x29, 0x74, 0x24, 0x20, 0x44, 0x0f, 0x29, 0x4c, 0x24,
                0x30, 0x44, 0x0f, 0x29, 0x54, 0x24, 0x40, 0x44, 0x0f, 0x29, 0x5c, 0x24, 0x50, 0x44,
                0x0f, 0x29, 0x64, 0x24, 0x60,
                // RCX points to six initialized floats: vx,vy,vz,dt,sign,tolerance.
                0xf3, 0x44, 0x0f, 0x10, 0x11, 0xf3, 0x0f, 0x10, 0x71, 0x04, 0xf3, 0x44, 0x0f, 0x10,
                0x49, 0x08, 0xf3, 0x0f, 0x10, 0x51, 0x0c, 0xf3, 0x44, 0x0f, 0x10, 0x59, 0x10, 0xf3,
                0x44, 0x0f, 0x10, 0x61, 0x14,
            ];
            let start = code.len();
            code.extend_from_slice(calculation);
            // Reached the ordinary debit/exhaustion body. Its native behavior is not emulated:
            // the fixture records admission and local distances for an independent cost check.
            code.extend_from_slice(&[0xc7, 0x02, 1, 0, 0, 0, 0xe9, 0, 0, 0, 0]);
            let body_jump = code.len() - 4;
            let next = start + SIZE + 0x223;
            code.resize(next, 0xcc);
            // The real zero-motion destination advances to the next entity.
            code.extend_from_slice(&[0xc7, 0x02, 0, 0, 0, 0]);
            let common = code.len();
            code[body_jump..body_jump + 4].copy_from_slice(
                &i32::try_from(common - (body_jump + 4))
                    .unwrap()
                    .to_le_bytes(),
            );
            code.extend_from_slice(&[
                0xf3, 0x44, 0x0f, 0x11, 0x52, 0x04, 0xf3, 0x0f, 0x11, 0x72, 0x08, 0xf3, 0x44, 0x0f,
                0x11, 0x4a, 0x0c, 0xf3, 0x0f, 0x11, 0x52, 0x10, 0x0f, 0x28, 0x74, 0x24, 0x20, 0x44,
                0x0f, 0x28, 0x4c, 0x24, 0x30, 0x44, 0x0f, 0x28, 0x54, 0x24, 0x40, 0x44, 0x0f, 0x28,
                0x5c, 0x24, 0x50, 0x44, 0x0f, 0x28, 0x64, 0x24, 0x60, 0x48, 0x83, 0xc4, 0x78, 0xc3,
            ]);
            let memory = Mem::new(4096, 0x3000, 0x40);
            // SAFETY: new private executable pages, all zero over this full range. The checked
            // write flushes the instruction cache. They live through every synchronous call.
            unsafe { emberkit::patch::write_checked(memory.base, &vec![0; code.len()], &code) }
                .unwrap();
            Harness { memory }
        }

        /// Execute this inert calculation and return admission, distance components and unchanged dt.
        fn run(&self, velocity: [f32; 3], dt: f32) -> (bool, [f32; 3], f32) {
            type Run = unsafe extern "system" fn(*const f32, *mut u32);
            // SAFETY: the owned wrapper implements this ABI and preserves the nonvolatile state.
            // It touches only these complete input/output arrays and its private frame.
            let run = unsafe { std::mem::transmute::<usize, Run>(self.memory.base) };
            let input = [velocity[0], velocity[1], velocity[2], dt, -0.0, 1e-20];
            let mut output = [0u32; 5];
            // SAFETY: complete initialized arrays stay alive and unaliased for this call.
            unsafe { run(input.as_ptr(), output.as_mut_ptr()) };
            (
                output[0] != 0,
                [
                    f32::from_bits(output[1]),
                    f32::from_bits(output[2]),
                    f32::from_bits(output[3]),
                ],
                f32::from_bits(output[4]),
            )
        }
    }

    /// Use the native scalar order for the independent debit comparison.
    fn distance(vector: [f32; 3]) -> f32 {
        ((vector[1] * vector[1] + vector[0] * vector[0]) + vector[2] * vector[2]).sqrt()
    }

    /// Motionless clinging must enter the native debit and exhaustion body, rather than hang free.
    #[test]
    fn stationary_hanging_has_a_small_native_effort() {
        assert!(
            std::is_x86_feature_detected!("sse4.1"),
            "native-code regression needs SSE4.1"
        );
        let original = Harness::new(&GAME);
        let owned = Harness::new(&HOLD);
        assert!(!original.run([0.0; 3], 1.0 / 60.0).0);
        for hz in [20.0, 60.0, 120.0] {
            let dt = 1.0 / hz;
            let (debit, vector, returned_dt) = owned.run([0.0; 3], dt);
            assert!(debit, "idle at{hz}Hz must reach native exhaustion/debit");
            assert_eq!(returned_dt.to_bits(), dt.to_bits());
            assert!((distance(vector) * 8.0 * hz - 4.0).abs() < 1e-5);
        }
    }

    /// The new instruction is a startup prerequisite, independent of host test capabilities.
    #[test]
    fn an_unsupported_cpu_refuses_all_climbing_writes() {
        assert!(require_cpu(false).unwrap_err().contains("nothing patched"));
        assert_eq!(require_cpu(true), Ok(()));
    }

    /// Both byte arrays decode completely, including every branch.
    #[test]
    fn calculations_decode_completely() {
        for code in [&GAME, &HOLD] {
            let instructions: Vec<_> = Decoder::with_ip(64, code, 0, DecoderOptions::NONE)
                .iter()
                .collect();
            assert!(instructions
                .iter()
                .all(|instruction| !instruction.is_invalid()));
            assert_eq!(
                instructions
                    .iter()
                    .map(|instruction| instruction.len())
                    .sum::<usize>(),
                SIZE
            );
        }
    }

    /// Movement above the minimum keeps the original distances byte for byte, with no second fee.
    #[test]
    fn normal_movement_keeps_the_native_charge() {
        let original = Harness::new(&GAME);
        let owned = Harness::new(&HOLD);
        for dt in [1.0 / 20.0, 1.0 / 60.0, 1.0 / 120.0, 0.25, -1.0 / 60.0] {
            for velocity in [
                [1.95, 0.0, 0.0],
                [0.0, -1.95, 0.0],
                [0.0, 0.0, 1.95],
                [0.4, 0.4, 0.4],
                [-0.3, 0.0, 0.5],
                [0.0, 0.5001, 0.0],
            ] {
                let before = original.run(velocity, dt);
                let after = owned.run(velocity, dt);
                assert!(before.0 && after.0);
                assert_eq!(
                    after.1.map(f32::to_bits),
                    before.1.map(f32::to_bits),
                    "{velocity:?} dt{dt}"
                );
                assert_eq!(after.2.to_bits(), dt.to_bits());
            }
        }
    }

    /// A tiny stick input cannot avoid the hanging cost, and moving never pays it twice.
    #[test]
    fn slow_movement_uses_the_greater_of_motion_and_minimum_effort() {
        let owned = Harness::new(&HOLD);
        let mut seed = 0x5f37_a2d9u32;
        for step in 0..2500 {
            let mut velocity = [0.0; 3];
            for axis in &mut velocity {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *axis = ((seed >> 8) as f32 / 16_777_216.0 - 0.5) * 2.0;
            }
            if step % 7 == 0 {
                velocity = [0.0; 3];
            }
            let dt = [1.0 / 20.0, 1.0 / 60.0, 1.0 / 120.0][step % 3];
            let (debit, actual, returned_dt) = owned.run(velocity, dt);
            assert!(debit);
            let native = velocity.map(|axis| axis * dt);
            let expected = distance(native).max(0.5 * dt);
            assert!(
                (distance(actual) - expected).abs() < 1e-7,
                "{velocity:?} dt{dt}"
            );
            assert_eq!(returned_dt.to_bits(), dt.to_bits());
        }
    }

    /// A paused zero-duration tick does not debit or trigger the exhaustion body.
    #[test]
    fn zero_duration_keeps_the_native_skip() {
        let original = Harness::new(&GAME);
        let owned = Harness::new(&HOLD);
        for velocity in [[0.0; 3], [1.95, -1.95, 0.75]] {
            assert!(!original.run(velocity, 0.0).0);
            let after = owned.run(velocity, 0.0);
            assert!(!after.0);
            assert_eq!(after.2.to_bits(), 0.0f32.to_bits());
        }
    }

    /// Native integer-nanosecond endpoints remain normal floats and preserve the effort rate.
    #[test]
    fn native_duration_endpoints_and_reverse_ticks_are_finite() {
        let owned = Harness::new(&HOLD);
        for nanos in [1i64, -1, 8_333_333, -16_666_667, i64::MAX, i64::MIN] {
            let dt = (nanos as f64 * 1e-9) as f32;
            assert!(dt.is_normal());
            let (debit, vector, returned_dt) = owned.run([0.0; 3], dt);
            assert!(debit, "{nanos}ns");
            assert!(vector.into_iter().all(f32::is_finite));
            assert!((distance(vector) / dt.abs() - 0.5).abs() < 1e-6);
            assert_eq!(returned_dt.to_bits(), dt.to_bits());
        }
    }

    /// An invalid velocity follows the same native body and keeps its local component bits.
    #[test]
    fn nonfinite_velocity_is_not_replaced_with_a_fictitious_hold() {
        let original = Harness::new(&GAME);
        let owned = Harness::new(&HOLD);
        for bad in [
            f32::from_bits(0x7fc0_1234),
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            for axis in 0..3 {
                let mut velocity = [0.0; 3];
                velocity[axis] = bad;
                let before = original.run(velocity, 1.0 / 60.0);
                let after = owned.run(velocity, 1.0 / 60.0);
                assert_eq!(after.0, before.0);
                assert_eq!(after.1.map(f32::to_bits), before.1.map(f32::to_bits));
            }
        }
    }

    /// All branches join original blocks; the inline math reads no memory and changes no stack.
    #[test]
    fn owned_math_has_only_the_proven_registers_and_destinations() {
        use iced_x86::{Mnemonic, OpKind, Register};
        let instructions: Vec<_> = Decoder::with_ip(64, &HOLD, 0, DecoderOptions::NONE)
            .iter()
            .collect();
        let targets: Vec<_> = instructions
            .iter()
            .filter(|instruction| instruction.op0_kind() == OpKind::NearBranch64)
            .map(|instruction| instruction.near_branch_target())
            .collect();
        assert_eq!(targets, [61, SIZE as u64, (SIZE + 0x223) as u64]);
        for instruction in instructions {
            assert!(!instruction.is_ip_rel_memory_operand());
            assert!(!(0..instruction.op_count())
                .any(|operand| instruction.op_kind(operand) == OpKind::Memory));
            assert!(!matches!(
                instruction.mnemonic(),
                Mnemonic::Call | Mnemonic::Ret | Mnemonic::Push | Mnemonic::Pop
            ));
            for operand in 0..instruction.op_count() {
                if instruction.op_kind(operand) == OpKind::Register {
                    assert!(matches!(
                        instruction.op_register(operand),
                        Register::EAX
                            | Register::XMM0
                            | Register::XMM1
                            | Register::XMM2
                            | Register::XMM6
                            | Register::XMM9
                            | Register::XMM10
                            | Register::XMM12
                    ));
                }
            }
        }
        assert_eq!(
            &HOLD[35..39],
            [0x85, 0xc0, 0x74, 0x16],
            "TEST clears CF before zero-duration JZ"
        );
    }
}
