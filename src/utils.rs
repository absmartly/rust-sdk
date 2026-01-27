use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

use crate::md5::md5;

pub fn base64_url_no_padding(data: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(data)
}

pub fn hash_unit(value: &str) -> String {
    let hash = md5(value.as_bytes());
    base64_url_no_padding(&hash)
}

pub fn choose_variant(split: &[f64], prob: f64) -> usize {
    let mut cum_sum = 0.0;
    for (i, &weight) in split.iter().enumerate() {
        cum_sum += weight;
        if prob < cum_sum {
            return i;
        }
    }
    split.len().saturating_sub(1)
}

pub fn array_equals_shallow<T: PartialEq>(a: &[T], b: &[T]) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x == y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_url_no_padding() {
        assert_eq!(base64_url_no_padding(b""), "");
        assert_eq!(base64_url_no_padding(b" "), "IA");
        assert_eq!(base64_url_no_padding(b"t"), "dA");
        assert_eq!(base64_url_no_padding(b"te"), "dGU");
        assert_eq!(base64_url_no_padding(b"tes"), "dGVz");
        assert_eq!(base64_url_no_padding(b"test"), "dGVzdA");
        assert_eq!(base64_url_no_padding(b"testy"), "dGVzdHk");
        assert_eq!(base64_url_no_padding(b"testy1"), "dGVzdHkx");
        assert_eq!(base64_url_no_padding(b"testy12"), "dGVzdHkxMg");
        assert_eq!(base64_url_no_padding(b"testy123"), "dGVzdHkxMjM");
        assert_eq!(
            base64_url_no_padding(b"The quick brown fox jumps over the lazy dog"),
            "VGhlIHF1aWNrIGJyb3duIGZveCBqdW1wcyBvdmVyIHRoZSBsYXp5IGRvZw"
        );
    }

    #[test]
    fn test_base64_url_no_padding_special_chars() {
        let special = "special characters açb↓c".as_bytes();
        assert_eq!(base64_url_no_padding(special), "c3BlY2lhbCBjaGFyYWN0ZXJzIGHDp2LihpNj");
    }

    #[test]
    fn test_hash_unit_known_values() {
        assert_eq!(
            hash_unit("4a42766ca6313d26f49985e799ff4f3790fb86efa0fce46edb3ea8fbf1ea3408"),
            "H2jvj6o9YcAgNdhKqEbtWw"
        );
        assert_eq!(hash_unit("bleh@absmarty.com"), "DRgslOje35bZMmpaohQjkA");
        assert_eq!(hash_unit("açb↓c"), "LxcqH5VC15rXfWfA_smreg");
        assert_eq!(hash_unit("testy"), "K5I_V6RgP8c6sYKz-TVn8g");
    }

    #[test]
    fn test_hash_unit_no_padding_or_special_chars() {
        let hash = hash_unit("test123");
        assert!(!hash.is_empty());
        assert!(!hash.contains('+'));
        assert!(!hash.contains('/'));
        assert!(!hash.contains('='));
    }

    #[test]
    fn test_choose_variant_two_way_split_zero_first() {
        assert_eq!(choose_variant(&[0.0, 1.0], 0.0), 1);
        assert_eq!(choose_variant(&[0.0, 1.0], 0.5), 1);
        assert_eq!(choose_variant(&[0.0, 1.0], 1.0), 1);
    }

    #[test]
    fn test_choose_variant_two_way_split_full_first() {
        assert_eq!(choose_variant(&[1.0, 0.0], 0.0), 0);
        assert_eq!(choose_variant(&[1.0, 0.0], 0.5), 0);
        assert_eq!(choose_variant(&[1.0, 0.0], 1.0), 1);
    }

    #[test]
    fn test_choose_variant_two_way_split_even() {
        assert_eq!(choose_variant(&[0.5, 0.5], 0.0), 0);
        assert_eq!(choose_variant(&[0.5, 0.5], 0.25), 0);
        assert_eq!(choose_variant(&[0.5, 0.5], 0.49999999), 0);
        assert_eq!(choose_variant(&[0.5, 0.5], 0.5), 1);
        assert_eq!(choose_variant(&[0.5, 0.5], 0.50000001), 1);
        assert_eq!(choose_variant(&[0.5, 0.5], 0.75), 1);
        assert_eq!(choose_variant(&[0.5, 0.5], 1.0), 1);
    }

    #[test]
    fn test_choose_variant_three_way_split() {
        assert_eq!(choose_variant(&[0.333, 0.333, 0.334], 0.0), 0);
        assert_eq!(choose_variant(&[0.333, 0.333, 0.334], 0.25), 0);
        assert_eq!(choose_variant(&[0.333, 0.333, 0.334], 0.33299999), 0);
        assert_eq!(choose_variant(&[0.333, 0.333, 0.334], 0.333), 1);
        assert_eq!(choose_variant(&[0.333, 0.333, 0.334], 0.33300001), 1);
        assert_eq!(choose_variant(&[0.333, 0.333, 0.334], 0.5), 1);
        assert_eq!(choose_variant(&[0.333, 0.333, 0.334], 0.66599999), 1);
        assert_eq!(choose_variant(&[0.333, 0.333, 0.334], 0.666), 2);
        assert_eq!(choose_variant(&[0.333, 0.333, 0.334], 0.66600001), 2);
        assert_eq!(choose_variant(&[0.333, 0.333, 0.334], 0.75), 2);
        assert_eq!(choose_variant(&[0.333, 0.333, 0.334], 1.0), 2);
    }

    #[test]
    fn test_array_equals_shallow() {
        assert!(array_equals_shallow::<i32>(&[], &[]));
        assert!(array_equals_shallow(&[1], &[1]));
        assert!(array_equals_shallow(&[1, 2, 3], &[1, 2, 3]));
        assert!(array_equals_shallow(&[0.5, 1.0, 1.5], &[0.5, 1.0, 1.5]));

        assert!(!array_equals_shallow(&[1], &[]));
        assert!(!array_equals_shallow(&[], &[1]));
        assert!(!array_equals_shallow(&[1, 2, 3], &[1, 2]));
        assert!(!array_equals_shallow(&[1, 2], &[1, 2, 3]));
    }
}
