use crate::murmur3::murmur3_32;
use crate::utils::choose_variant;

pub struct VariantAssigner {
    unit_hash: u32,
}

impl VariantAssigner {
    pub fn new(unit: &str) -> Self {
        let unit_hash = murmur3_32(unit.as_bytes(), 0);
        Self { unit_hash }
    }

    pub fn assign(&self, split: &[f64], seed_hi: u32, seed_lo: u32) -> usize {
        let prob = self.probability(seed_hi, seed_lo);
        choose_variant(split, prob)
    }

    fn probability(&self, seed_hi: u32, seed_lo: u32) -> f64 {
        let mut buffer = [0u8; 12];
        buffer[0..4].copy_from_slice(&seed_lo.to_le_bytes());
        buffer[4..8].copy_from_slice(&seed_hi.to_le_bytes());
        buffer[8..12].copy_from_slice(&self.unit_hash.to_le_bytes());

        let hash = murmur3_32(&buffer, 0);
        (hash as f64) / (0xFFFFFFFFu32 as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::{choose_variant, hash_unit};

    #[test]
    fn test_choose_variant_returns_correct_index() {
        assert_eq!(choose_variant(&[0.0, 1.0], 0.0), 1);
        assert_eq!(choose_variant(&[0.0, 1.0], 0.5), 1);
        assert_eq!(choose_variant(&[1.0, 0.0], 0.0), 0);
        assert_eq!(choose_variant(&[0.5, 0.5], 0.0), 0);
        assert_eq!(choose_variant(&[0.5, 0.5], 0.25), 0);
        assert_eq!(choose_variant(&[0.5, 0.5], 0.49999999), 0);
        assert_eq!(choose_variant(&[0.5, 0.5], 0.5), 1);
        assert_eq!(choose_variant(&[0.5, 0.5], 0.50000001), 1);
        assert_eq!(choose_variant(&[0.333, 0.333, 0.334], 0.0), 0);
        assert_eq!(choose_variant(&[0.333, 0.333, 0.334], 0.333), 1);
        assert_eq!(choose_variant(&[0.333, 0.333, 0.334], 0.666), 2);
    }

    macro_rules! assignment_test {
        ($name:ident, $unit:expr, $split:expr, $seed_hi:expr, $seed_lo:expr, $expected:expr) => {
            #[test]
            fn $name() {
                let hashed_unit = hash_unit($unit);
                let assigner = VariantAssigner::new(&hashed_unit);
                assert_eq!(assigner.assign($split, $seed_hi, $seed_lo), $expected);
            }
        };
    }

    assignment_test!(
        test_bleh_binary_s0_s0,
        "bleh@absmartly.com",
        &[0.5, 0.5],
        0x00000000,
        0x00000000,
        0
    );
    assignment_test!(
        test_bleh_binary_s0_s1,
        "bleh@absmartly.com",
        &[0.5, 0.5],
        0x00000000,
        0x00000001,
        1
    );
    assignment_test!(
        test_bleh_binary_sp1,
        "bleh@absmartly.com",
        &[0.5, 0.5],
        0x8015406f,
        0x7ef49b98,
        0
    );
    assignment_test!(
        test_bleh_binary_sp2,
        "bleh@absmartly.com",
        &[0.5, 0.5],
        0x3b2e7d90,
        0xca87df4d,
        0
    );
    assignment_test!(
        test_bleh_binary_sp3,
        "bleh@absmartly.com",
        &[0.5, 0.5],
        0x52c1f657,
        0xd248bb2e,
        0
    );
    assignment_test!(
        test_bleh_binary_sp4,
        "bleh@absmartly.com",
        &[0.5, 0.5],
        0x865a84d0,
        0xaa22d41a,
        0
    );
    assignment_test!(
        test_bleh_binary_sp5,
        "bleh@absmartly.com",
        &[0.5, 0.5],
        0x27d1dc86,
        0x845461b9,
        1
    );

    assignment_test!(
        test_bleh_three_s0_s0,
        "bleh@absmartly.com",
        &[0.33, 0.33, 0.34],
        0x00000000,
        0x00000000,
        0
    );
    assignment_test!(
        test_bleh_three_s0_s1,
        "bleh@absmartly.com",
        &[0.33, 0.33, 0.34],
        0x00000000,
        0x00000001,
        2
    );
    assignment_test!(
        test_bleh_three_sp1,
        "bleh@absmartly.com",
        &[0.33, 0.33, 0.34],
        0x8015406f,
        0x7ef49b98,
        0
    );
    assignment_test!(
        test_bleh_three_sp2,
        "bleh@absmartly.com",
        &[0.33, 0.33, 0.34],
        0x3b2e7d90,
        0xca87df4d,
        0
    );
    assignment_test!(
        test_bleh_three_sp3,
        "bleh@absmartly.com",
        &[0.33, 0.33, 0.34],
        0x52c1f657,
        0xd248bb2e,
        0
    );
    assignment_test!(
        test_bleh_three_sp4,
        "bleh@absmartly.com",
        &[0.33, 0.33, 0.34],
        0x865a84d0,
        0xaa22d41a,
        1
    );
    assignment_test!(
        test_bleh_three_sp5,
        "bleh@absmartly.com",
        &[0.33, 0.33, 0.34],
        0x27d1dc86,
        0x845461b9,
        1
    );

    assignment_test!(
        test_num_binary_s0_s0,
        "123456789",
        &[0.5, 0.5],
        0x00000000,
        0x00000000,
        1
    );
    assignment_test!(
        test_num_binary_s0_s1,
        "123456789",
        &[0.5, 0.5],
        0x00000000,
        0x00000001,
        0
    );
    assignment_test!(
        test_num_binary_sp1,
        "123456789",
        &[0.5, 0.5],
        0x8015406f,
        0x7ef49b98,
        1
    );
    assignment_test!(
        test_num_binary_sp2,
        "123456789",
        &[0.5, 0.5],
        0x3b2e7d90,
        0xca87df4d,
        1
    );
    assignment_test!(
        test_num_binary_sp3,
        "123456789",
        &[0.5, 0.5],
        0x52c1f657,
        0xd248bb2e,
        1
    );
    assignment_test!(
        test_num_binary_sp4,
        "123456789",
        &[0.5, 0.5],
        0x865a84d0,
        0xaa22d41a,
        0
    );
    assignment_test!(
        test_num_binary_sp5,
        "123456789",
        &[0.5, 0.5],
        0x27d1dc86,
        0x845461b9,
        0
    );

    assignment_test!(
        test_num_three_s0_s0,
        "123456789",
        &[0.33, 0.33, 0.34],
        0x00000000,
        0x00000000,
        2
    );
    assignment_test!(
        test_num_three_s0_s1,
        "123456789",
        &[0.33, 0.33, 0.34],
        0x00000000,
        0x00000001,
        1
    );
    assignment_test!(
        test_num_three_sp1,
        "123456789",
        &[0.33, 0.33, 0.34],
        0x8015406f,
        0x7ef49b98,
        2
    );
    assignment_test!(
        test_num_three_sp2,
        "123456789",
        &[0.33, 0.33, 0.34],
        0x3b2e7d90,
        0xca87df4d,
        2
    );
    assignment_test!(
        test_num_three_sp3,
        "123456789",
        &[0.33, 0.33, 0.34],
        0x52c1f657,
        0xd248bb2e,
        2
    );
    assignment_test!(
        test_num_three_sp4,
        "123456789",
        &[0.33, 0.33, 0.34],
        0x865a84d0,
        0xaa22d41a,
        0
    );
    assignment_test!(
        test_num_three_sp5,
        "123456789",
        &[0.33, 0.33, 0.34],
        0x27d1dc86,
        0x845461b9,
        0
    );

    assignment_test!(
        test_hash_binary_s0_s0,
        "e791e240fcd3df7d238cfc285f475e8152fcc0ec",
        &[0.5, 0.5],
        0x00000000,
        0x00000000,
        1
    );
    assignment_test!(
        test_hash_binary_s0_s1,
        "e791e240fcd3df7d238cfc285f475e8152fcc0ec",
        &[0.5, 0.5],
        0x00000000,
        0x00000001,
        0
    );
    assignment_test!(
        test_hash_binary_sp1,
        "e791e240fcd3df7d238cfc285f475e8152fcc0ec",
        &[0.5, 0.5],
        0x8015406f,
        0x7ef49b98,
        1
    );
    assignment_test!(
        test_hash_binary_sp2,
        "e791e240fcd3df7d238cfc285f475e8152fcc0ec",
        &[0.5, 0.5],
        0x3b2e7d90,
        0xca87df4d,
        1
    );
    assignment_test!(
        test_hash_binary_sp3,
        "e791e240fcd3df7d238cfc285f475e8152fcc0ec",
        &[0.5, 0.5],
        0x52c1f657,
        0xd248bb2e,
        0
    );
    assignment_test!(
        test_hash_binary_sp4,
        "e791e240fcd3df7d238cfc285f475e8152fcc0ec",
        &[0.5, 0.5],
        0x865a84d0,
        0xaa22d41a,
        0
    );
    assignment_test!(
        test_hash_binary_sp5,
        "e791e240fcd3df7d238cfc285f475e8152fcc0ec",
        &[0.5, 0.5],
        0x27d1dc86,
        0x845461b9,
        0
    );

    assignment_test!(
        test_hash_three_s0_s0,
        "e791e240fcd3df7d238cfc285f475e8152fcc0ec",
        &[0.33, 0.33, 0.34],
        0x00000000,
        0x00000000,
        2
    );
    assignment_test!(
        test_hash_three_s0_s1,
        "e791e240fcd3df7d238cfc285f475e8152fcc0ec",
        &[0.33, 0.33, 0.34],
        0x00000000,
        0x00000001,
        0
    );
    assignment_test!(
        test_hash_three_sp1,
        "e791e240fcd3df7d238cfc285f475e8152fcc0ec",
        &[0.33, 0.33, 0.34],
        0x8015406f,
        0x7ef49b98,
        2
    );
    assignment_test!(
        test_hash_three_sp2,
        "e791e240fcd3df7d238cfc285f475e8152fcc0ec",
        &[0.33, 0.33, 0.34],
        0x3b2e7d90,
        0xca87df4d,
        1
    );
    assignment_test!(
        test_hash_three_sp3,
        "e791e240fcd3df7d238cfc285f475e8152fcc0ec",
        &[0.33, 0.33, 0.34],
        0x52c1f657,
        0xd248bb2e,
        0
    );
    assignment_test!(
        test_hash_three_sp4,
        "e791e240fcd3df7d238cfc285f475e8152fcc0ec",
        &[0.33, 0.33, 0.34],
        0x865a84d0,
        0xaa22d41a,
        0
    );
    assignment_test!(
        test_hash_three_sp5,
        "e791e240fcd3df7d238cfc285f475e8152fcc0ec",
        &[0.33, 0.33, 0.34],
        0x27d1dc86,
        0x845461b9,
        1
    );
}
