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
    (a << b) | (a >> (32 - b))
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

    #[test]
    fn test_empty_string() {
        assert_eq!(murmur3_32(b"", 0), 0x00000000);
    }

    #[test]
    fn test_space() {
        assert_eq!(murmur3_32(b" ", 0), 0x7ef49b98);
    }

    #[test]
    fn test_single_char() {
        assert_eq!(murmur3_32(b"t", 0), 0xca87df4d);
    }

    #[test]
    fn test_two_chars() {
        assert_eq!(murmur3_32(b"te", 0), 0xedb8ee1b);
    }

    #[test]
    fn test_three_chars() {
        assert_eq!(murmur3_32(b"tes", 0), 0x0bb90e5a);
    }

    #[test]
    fn test_four_chars() {
        assert_eq!(murmur3_32(b"test", 0), 0xba6bd213);
    }

    #[test]
    fn test_with_seed_deadbeef() {
        assert_eq!(murmur3_32(b"test", 0xdeadbeef), 0xaa22d41a);
    }

    #[test]
    fn test_with_seed_1() {
        assert_eq!(murmur3_32(b"test", 1), 0x99c02ae2);
    }

    #[test]
    fn test_long_string() {
        assert_eq!(
            murmur3_32(b"The quick brown fox jumps over the lazy dog", 0),
            0x2e4ff723
        );
    }
}
