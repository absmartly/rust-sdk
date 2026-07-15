const C1: u32 = 0xcc9e2d51;
const C2: u32 = 0x1b873593;
const C3: u32 = 0xe6546b64;

fn fmix32(mut h: u32) -> u32 {
    h ^= h >> 16;
    h = h.wrapping_mul(0x85ebca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2ae35);
    h ^= h >> 16;
    h
}

fn rotl32(a: u32, b: u32) -> u32 {
    a.rotate_left(b)
}

fn scramble32(block: u32) -> u32 {
    rotl32(block.wrapping_mul(C1), 15).wrapping_mul(C2)
}

pub fn murmur3_32(key: &[u8], seed: u32) -> u32 {
    let mut hash = seed;

    let n = key.len() & !3;
    let mut i = 0;

    while i < n {
        let chunk = u32::from_le_bytes([key[i], key[i + 1], key[i + 2], key[i + 3]]);
        hash ^= scramble32(chunk);
        hash = rotl32(hash, 13);
        hash = hash.wrapping_mul(5).wrapping_add(C3);
        i += 4;
    }

    let mut remaining: u32 = 0;
    match key.len() & 3 {
        3 => {
            remaining ^= (key[i + 2] as u32) << 16;
            remaining ^= (key[i + 1] as u32) << 8;
            remaining ^= key[i] as u32;
            hash ^= scramble32(remaining);
        }
        2 => {
            remaining ^= (key[i + 1] as u32) << 8;
            remaining ^= key[i] as u32;
            hash ^= scramble32(remaining);
        }
        1 => {
            remaining ^= key[i] as u32;
            hash ^= scramble32(remaining);
        }
        _ => {}
    }

    hash ^= key.len() as u32;
    fmix32(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! murmur3_test {
        ($name:ident, $input:expr, $seed:expr, $expected:expr) => {
            #[test]
            fn $name() {
                assert_eq!(murmur3_32($input.as_bytes(), $seed), $expected);
            }
        };
    }

    murmur3_test!(test_seed0_empty, "", 0x00000000, 0x00000000);
    murmur3_test!(test_seed0_space, " ", 0x00000000, 0x7ef49b98);
    murmur3_test!(test_seed0_t, "t", 0x00000000, 0xca87df4d);
    murmur3_test!(test_seed0_te, "te", 0x00000000, 0xedb8ee1b);
    murmur3_test!(test_seed0_tes, "tes", 0x00000000, 0x0bb90e5a);
    murmur3_test!(test_seed0_test, "test", 0x00000000, 0xba6bd213);
    murmur3_test!(test_seed0_testy, "testy", 0x00000000, 0x44af8342);
    murmur3_test!(test_seed0_testy1, "testy1", 0x00000000, 0x8a1a243a);
    murmur3_test!(test_seed0_testy12, "testy12", 0x00000000, 0x845461b9);
    murmur3_test!(test_seed0_testy123, "testy123", 0x00000000, 0x47628ac4);
    murmur3_test!(
        test_seed0_special,
        "special characters a\u{00e7}b\u{2193}c",
        0x00000000,
        0xbe83b140
    );
    murmur3_test!(
        test_seed0_fox,
        "The quick brown fox jumps over the lazy dog",
        0x00000000,
        0x2e4ff723
    );

    murmur3_test!(test_deadbeef_empty, "", 0xdeadbeef, 0x0de5c6a9);
    murmur3_test!(test_deadbeef_space, " ", 0xdeadbeef, 0x25acce43);
    murmur3_test!(test_deadbeef_t, "t", 0xdeadbeef, 0x3b15dcf8);
    murmur3_test!(test_deadbeef_te, "te", 0xdeadbeef, 0xac981332);
    murmur3_test!(test_deadbeef_tes, "tes", 0xdeadbeef, 0xc1c78dda);
    murmur3_test!(test_deadbeef_test, "test", 0xdeadbeef, 0xaa22d41a);
    murmur3_test!(test_deadbeef_testy, "testy", 0xdeadbeef, 0x84f5f623);
    murmur3_test!(test_deadbeef_testy1, "testy1", 0xdeadbeef, 0x09ed28e9);
    murmur3_test!(test_deadbeef_testy12, "testy12", 0xdeadbeef, 0x22467835);
    murmur3_test!(test_deadbeef_testy123, "testy123", 0xdeadbeef, 0xd633060d);
    murmur3_test!(
        test_deadbeef_special,
        "special characters a\u{00e7}b\u{2193}c",
        0xdeadbeef,
        0xf7fdd8a2
    );
    murmur3_test!(
        test_deadbeef_fox,
        "The quick brown fox jumps over the lazy dog",
        0xdeadbeef,
        0x3a7b3f4d
    );

    murmur3_test!(test_seed1_empty, "", 0x00000001, 0x514e28b7);
    murmur3_test!(test_seed1_space, " ", 0x00000001, 0x4f0f7132);
    murmur3_test!(test_seed1_t, "t", 0x00000001, 0x5db1831e);
    murmur3_test!(test_seed1_te, "te", 0x00000001, 0xd248bb2e);
    murmur3_test!(test_seed1_tes, "tes", 0x00000001, 0xd432eb74);
    murmur3_test!(test_seed1_test, "test", 0x00000001, 0x99c02ae2);
    murmur3_test!(test_seed1_testy, "testy", 0x00000001, 0xc5b2dc1e);
    murmur3_test!(test_seed1_testy1, "testy1", 0x00000001, 0x33925ceb);
    murmur3_test!(test_seed1_testy12, "testy12", 0x00000001, 0xd92c9f23);
    murmur3_test!(test_seed1_testy123, "testy123", 0x00000001, 0x3bc1712d);
    murmur3_test!(
        test_seed1_special,
        "special characters a\u{00e7}b\u{2193}c",
        0x00000001,
        0x293327b5
    );
    murmur3_test!(
        test_seed1_fox,
        "The quick brown fox jumps over the lazy dog",
        0x00000001,
        0x78e69e27
    );
}
