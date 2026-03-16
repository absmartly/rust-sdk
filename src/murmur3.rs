//! `MurmurHash3` (32-bit) implementation used for deterministic unit assignment.

const C1: u32 = 0xcc9e_2d51;
const C2: u32 = 0x1b87_3593;
const C3: u32 = 0xe654_6b64;

fn fmix32(mut h: u32) -> u32 {
    h ^= h >> 16;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    h
}

fn rotl32(a: u32, b: u32) -> u32 {
    (a << b) | (a >> (32 - b))
}

fn scramble32(block: u32) -> u32 {
    rotl32(block.wrapping_mul(C1), 15).wrapping_mul(C2)
}

/// Computes a 32-bit `MurmurHash3` of the given byte slice with the specified seed.
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
            remaining ^= u32::from(key[i + 2]) << 16;
            remaining ^= u32::from(key[i + 1]) << 8;
            remaining ^= u32::from(key[i]);
            hash ^= scramble32(remaining);
        }
        2 => {
            remaining ^= u32::from(key[i + 1]) << 8;
            remaining ^= u32::from(key[i]);
            hash ^= scramble32(remaining);
        }
        1 => {
            remaining ^= u32::from(key[i]);
            hash ^= scramble32(remaining);
        }
        _ => {}
    }

    #[allow(clippy::cast_possible_truncation)]
    let len = key.len() as u32;
    hash ^= len;
    fmix32(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_string() {
        assert_eq!(murmur3_32(b"", 0), 0x0000_0000);
    }

    #[test]
    fn test_space() {
        assert_eq!(murmur3_32(b" ", 0), 0x7ef4_9b98);
    }

    #[test]
    fn test_single_char() {
        assert_eq!(murmur3_32(b"t", 0), 0xca87_df4d);
    }

    #[test]
    fn test_two_chars() {
        assert_eq!(murmur3_32(b"te", 0), 0xedb8_ee1b);
    }

    #[test]
    fn test_three_chars() {
        assert_eq!(murmur3_32(b"tes", 0), 0x0bb9_0e5a);
    }

    #[test]
    fn test_four_chars() {
        assert_eq!(murmur3_32(b"test", 0), 0xba6b_d213);
    }

    #[test]
    fn test_with_seed_deadbeef() {
        assert_eq!(murmur3_32(b"test", 0xdead_beef), 0xaa22_d41a);
    }

    #[test]
    fn test_with_seed_1() {
        assert_eq!(murmur3_32(b"test", 1), 0x99c0_2ae2);
    }

    #[test]
    fn test_long_string() {
        assert_eq!(
            murmur3_32(b"The quick brown fox jumps over the lazy dog", 0),
            0x2e4f_f723
        );
    }
}
