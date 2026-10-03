// Falcon's independently written replacement, under Apache-2.0.
// It is derived from the matrix specification below, not from the former external snippet.

// A four-row quadrant is interleaved in pairs, then in pairs of pairs. Every output
// lane is therefore one complete column of that 4x4 quadrant.
#[inline(always)]
unsafe fn transpose_quadrant(rows: [__m128i; 4]) -> [__m128i; 4] {
    unsafe {
        let first_pair_low = _mm_unpacklo_epi32(rows[0], rows[1]);
        let first_pair_high = _mm_unpackhi_epi32(rows[0], rows[1]);
        let second_pair_low = _mm_unpacklo_epi32(rows[2], rows[3]);
        let second_pair_high = _mm_unpackhi_epi32(rows[2], rows[3]);
        [
            _mm_unpacklo_epi64(first_pair_low, second_pair_low),
            _mm_unpackhi_epi64(first_pair_low, second_pair_low),
            _mm_unpacklo_epi64(first_pair_high, second_pair_high),
            _mm_unpackhi_epi64(first_pair_high, second_pair_high),
        ]
    }
}

/// Transpose eight rows of eight i32 lanes: result[row][column] = input[column][row].
/// Every bit is moved unchanged. The caller must establish AVX2 support and provide eight
/// disjoint mutable registers, as required by Rust's reference rules.
#[target_feature(enable = "avx2")]
#[inline]
pub unsafe fn transpose(
    a: &mut YmmRegister,
    b: &mut YmmRegister,
    c: &mut YmmRegister,
    d: &mut YmmRegister,
    e: &mut YmmRegister,
    f: &mut YmmRegister,
    g: &mut YmmRegister,
    h: &mut YmmRegister,
) {
    let input = [
        a.mm256, b.mm256, c.mm256, d.mm256, e.mm256, f.mm256, g.mm256, h.mm256,
    ];
    let output = unsafe {
        let left = input.map(|row| _mm256_castsi256_si128(row));
        let right = input.map(|row| _mm256_extracti128_si256::<1>(row));
        let top_left = transpose_quadrant([left[0], left[1], left[2], left[3]]);
        let bottom_left = transpose_quadrant([left[4], left[5], left[6], left[7]]);
        let top_right = transpose_quadrant([right[0], right[1], right[2], right[3]]);
        let bottom_right = transpose_quadrant([right[4], right[5], right[6], right[7]]);
        let join = |top, bottom| _mm256_inserti128_si256::<1>(_mm256_castsi128_si256(top), bottom);
        [
            join(top_left[0], bottom_left[0]),
            join(top_left[1], bottom_left[1]),
            join(top_left[2], bottom_left[2]),
            join(top_left[3], bottom_left[3]),
            join(top_right[0], bottom_right[0]),
            join(top_right[1], bottom_right[1]),
            join(top_right[2], bottom_right[2]),
            join(top_right[3], bottom_right[3]),
        ]
    };
    [
        a.mm256, b.mm256, c.mm256, d.mm256, e.mm256, f.mm256, g.mm256, h.mm256,
    ] = output;
}

#[cfg(all(test, feature = "std"))]
mod falcon_transpose_tests {
    use super::{transpose, YmmRegister};

    fn available() -> bool {
        if !std::is_x86_feature_detected!("avx2") {
            eprintln!("SKIP Falcon transpose: AVX2 is unavailable");
            return false;
        }
        eprintln!("Falcon transpose: AVX2 kernel executed");
        true
    }

    fn run(input: [[i32; 8]; 8]) -> [[i32; 8]; 8] {
        // The tests check AVX2 before calling this helper. __m256i has no invalid bit patterns.
        let mut registers = input.map(|row| YmmRegister {
            mm256: unsafe { core::mem::transmute::<[i32; 8], _>(row) },
        });
        let [a, b, c, d, e, f, g, h] = &mut registers;
        unsafe { transpose(a, b, c, d, e, f, g, h) };
        registers.map(|register| unsafe { core::mem::transmute(register.mm256) })
    }

    #[test]
    fn coordinates_and_each_basis_lane_match_the_specification() {
        if !available() {
            return;
        }
        // Falsifiers: return identity, swap two output rows, or reverse the upper lanes.
        let input = core::array::from_fn(|row| core::array::from_fn(|col| (8 * row + col) as i32));
        let output = run(input);
        for (row, values) in output.iter().enumerate() {
            for (col, value) in values.iter().enumerate() {
                assert_eq!(*value, (8 * col + row) as i32);
            }
        }
        for position in 0..64 {
            for bit in [1_i32, i32::MIN, i32::MAX, -1] {
                let mut input = [[0; 8]; 8];
                input[position / 8][position % 8] = bit;
                let output = run(input);
                let destination = (position % 8) * 8 + position / 8;
                for (index, value) in output.into_iter().flatten().enumerate() {
                    assert_eq!(value, if index == destination { bit } else { 0 });
                }
            }
        }
    }

    #[test]
    fn arbitrary_bits_and_double_transpose_are_preserved() {
        if !available() {
            return;
        }
        // Falsifiers: truncate/sign-convert a lane, alter one high bit, or change the mapping.
        let mut state = 0x6285_73ab_u32;
        for _ in 0..256 {
            let input = core::array::from_fn(|_| {
                core::array::from_fn(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    state as i32
                })
            });
            let output = run(input);
            for row in 0..8 {
                for col in 0..8 {
                    assert_eq!(
                        output[row][col].to_ne_bytes(),
                        input[col][row].to_ne_bytes()
                    );
                }
            }
            assert_eq!(run(output), input);
        }
    }
}
