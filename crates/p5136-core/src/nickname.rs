//! Cross-platform nickname validation and case-insensitive identity keys.

use thiserror::Error;
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

pub const MAXIMUM_NICKNAME_LENGTH: usize = 32;

const RESERVED_WINDOWS_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum NicknameError {
    #[error("nickname cannot be empty")]
    Empty,

    #[error("nickname exceeds {maximum} UTF-16 code units")]
    TooLong { maximum: usize },

    #[error("nickname has an unsafe trailing path component")]
    UnsafePathComponent,

    #[error("nickname contains invalid character U+{codepoint:04X}")]
    InvalidCharacter { codepoint: u32 },

    #[error("nickname uses reserved Windows device name {0}")]
    ReservedWindowsName(String),
}

pub fn normalize_nickname(value: &str) -> Result<String, NicknameError> {
    let nickname = value.trim().nfc().collect::<String>();
    if nickname.is_empty() {
        return Err(NicknameError::Empty);
    }
    if nickname.encode_utf16().count() > MAXIMUM_NICKNAME_LENGTH {
        return Err(NicknameError::TooLong {
            maximum: MAXIMUM_NICKNAME_LENGTH,
        });
    }
    if nickname == "." || nickname == ".." || nickname.ends_with('.') || nickname.ends_with(' ') {
        return Err(NicknameError::UnsafePathComponent);
    }

    for character in nickname.chars() {
        let invalid_windows_punctuation = matches!(
            character,
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
        );
        if character.is_control() || invalid_windows_punctuation {
            return Err(NicknameError::InvalidCharacter {
                codepoint: u32::from(character),
            });
        }
    }

    let base_name = nickname.split('.').next().unwrap_or_default();
    if RESERVED_WINDOWS_NAMES
        .iter()
        .any(|reserved| base_name.eq_ignore_ascii_case(reserved))
    {
        return Err(NicknameError::ReservedWindowsName(base_name.to_owned()));
    }

    Ok(nickname)
}

#[must_use]
pub fn canonical_nickname_key(nickname: &str) -> String {
    nickname.nfc().case_fold().nfc().collect()
}

#[cfg(test)]
mod tests {
    use super::{NicknameError, canonical_nickname_key, normalize_nickname};
    use unicode_normalization::UnicodeNormalization;

    #[test]
    fn applies_windows_path_rules_on_every_host() {
        assert_eq!(normalize_nickname("  Yany2  ").unwrap(), "Yany2");
        assert!(matches!(
            normalize_nickname("con.profile"),
            Err(NicknameError::ReservedWindowsName(_))
        ));
        assert!(matches!(
            normalize_nickname("../escape"),
            Err(NicknameError::InvalidCharacter { .. })
        ));
        assert!(matches!(
            normalize_nickname("trailing."),
            Err(NicknameError::UnsafePathComponent)
        ));
    }

    #[test]
    fn canonical_key_is_case_insensitive_for_supported_names() {
        assert_eq!(
            canonical_nickname_key("RIDER"),
            canonical_nickname_key("rider")
        );
        assert_eq!(canonical_nickname_key("라이더"), "라이더");
    }

    #[test]
    fn normalizes_unicode_before_validation_and_after_case_mapping() {
        let composed = "\u{00e9}";
        let decomposed = "e\u{0301}";
        assert_eq!(normalize_nickname(composed).unwrap(), composed);
        assert_eq!(normalize_nickname(decomposed).unwrap(), composed);
        assert_eq!(
            canonical_nickname_key(composed),
            canonical_nickname_key(decomposed)
        );

        // U+0130 lowercases to `i` plus a combining dot. The canonical key
        // remains normalized even when case mapping expands one scalar.
        let expanded = canonical_nickname_key("\u{0130}");
        assert_eq!(expanded, expanded.nfc().collect::<String>());
    }

    #[test]
    fn canonical_key_uses_full_unicode_case_folding() {
        assert_eq!(
            canonical_nickname_key("\u{03c3}"),
            canonical_nickname_key("\u{03c2}")
        );
        assert_eq!(
            canonical_nickname_key("Stra\u{00df}e"),
            canonical_nickname_key("STRASSE")
        );
        assert_eq!(
            canonical_nickname_key("\u{017f}"),
            canonical_nickname_key("S")
        );
    }
}
