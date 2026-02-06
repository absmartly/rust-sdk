use md5::{Digest, Md5};

pub fn md5(data: &[u8]) -> [u8; 16] {
    let mut hasher = Md5::new();
    hasher.update(data);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::base64_url_no_padding;

    fn md5_base64(input: &str) -> String {
        base64_url_no_padding(&md5(input.as_bytes()))
    }

    macro_rules! md5_test {
        ($name:ident, $input:expr, $expected:expr) => {
            #[test]
            fn $name() {
                assert_eq!(md5_base64($input), $expected);
            }
        };
    }

    md5_test!(test_md5_empty_string, "", "1B2M2Y8AsgTpgAmY7PhCfg");
    md5_test!(test_md5_space, " ", "chXunH2dwinSkhpA6JnsXw");
    md5_test!(test_md5_t, "t", "41jvpIn1gGLxDdcxa2Vkng");
    md5_test!(test_md5_te, "te", "Vp73JkK-D63XEdakaNaO4Q");
    md5_test!(test_md5_tes, "tes", "KLZi2IO212_Zbk3cXpungA");
    md5_test!(test_md5_test, "test", "CY9rzUYh03PK3k6DJie09g");
    md5_test!(test_md5_testy, "testy", "K5I_V6RgP8c6sYKz-TVn8g");
    md5_test!(test_md5_testy1, "testy1", "8fT8xGipOhPkZ2DncKU-1A");
    md5_test!(test_md5_testy12, "testy12", "YqRAtOz000gIu61ErEH18A");
    md5_test!(test_md5_testy123, "testy123", "pfV2H07L6WvdqlY0zHuYIw");
    md5_test!(test_md5_special_characters, "special characters a\u{00e7}b\u{2193}c", "4PIrO7lKtTxOcj2eMYlG7A");
    md5_test!(test_md5_quick_brown_fox, "The quick brown fox jumps over the lazy dog", "nhB9nTcrtoJr2B01QqQZ1g");
    md5_test!(test_md5_quick_brown_fox_pie, "The quick brown fox jumps over the lazy dog and eats a pie", "iM-8ECRrLUQzixl436y96A");
    md5_test!(test_md5_lorem_ipsum, "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.", "24m7XOq4f5wPzCqzbBicLA");
}
