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
    use crate::utils::hash_unit;

    #[test]
    fn test_assigner_bleh_email() {
        let hashed_unit = hash_unit("bleh@absmartly.com");
        let assigner = VariantAssigner::new(&hashed_unit);
        let variant = assigner.assign(&[0.5, 0.5], 0x00000000, 0x00000000);
        assert_eq!(variant, 0);
    }

    #[test]
    fn test_assigner_bleh_email_different_seed() {
        let hashed_unit = hash_unit("bleh@absmartly.com");
        let assigner = VariantAssigner::new(&hashed_unit);
        let variant = assigner.assign(&[0.5, 0.5], 0x00000000, 0x00000001);
        assert_eq!(variant, 1);
    }

    #[test]
    fn test_assigner_123456789() {
        let hashed_unit = hash_unit("123456789");
        let assigner = VariantAssigner::new(&hashed_unit);
        assert_eq!(assigner.assign(&[0.5, 0.5], 0x00000000, 0x00000000), 1);
        assert_eq!(assigner.assign(&[0.5, 0.5], 0x00000000, 0x00000001), 0);
    }

    #[test]
    fn test_assigner_three_way_split() {
        let hashed_unit = hash_unit("bleh@absmartly.com");
        let assigner = VariantAssigner::new(&hashed_unit);
        let variant = assigner.assign(&[0.33, 0.33, 0.34], 0x00000000, 0x00000001);
        assert_eq!(variant, 2);
    }
}
