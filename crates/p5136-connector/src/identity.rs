use thiserror::Error;

pub const MAXIMUM_NICKNAME_LENGTH: usize = 32;

const RESERVED_WINDOWS_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum IdentityError {
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

pub fn normalize_nickname(value: &str) -> Result<String, IdentityError> {
    let nickname = value.trim();
    if nickname.is_empty() {
        return Err(IdentityError::Empty);
    }
    if nickname.encode_utf16().count() > MAXIMUM_NICKNAME_LENGTH {
        return Err(IdentityError::TooLong {
            maximum: MAXIMUM_NICKNAME_LENGTH,
        });
    }
    if nickname == "." || nickname == ".." || nickname.ends_with('.') || nickname.ends_with(' ') {
        return Err(IdentityError::UnsafePathComponent);
    }

    for character in nickname.chars() {
        let invalid_windows_punctuation = matches!(
            character,
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
        );
        if character.is_control() || invalid_windows_punctuation {
            return Err(IdentityError::InvalidCharacter {
                codepoint: u32::from(character),
            });
        }
    }

    let base_name = nickname.split('.').next().unwrap_or_default();
    if RESERVED_WINDOWS_NAMES
        .iter()
        .any(|reserved| base_name.eq_ignore_ascii_case(reserved))
    {
        return Err(IdentityError::ReservedWindowsName(base_name.to_owned()));
    }

    Ok(nickname.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{IdentityError, normalize_nickname};

    #[test]
    fn applies_windows_rules_on_every_host() {
        assert_eq!(normalize_nickname("  Yany2  ").unwrap(), "Yany2");
        assert!(matches!(
            normalize_nickname("con.profile"),
            Err(IdentityError::ReservedWindowsName(_))
        ));
        assert!(matches!(
            normalize_nickname("../escape"),
            Err(IdentityError::InvalidCharacter { .. })
        ));
        assert!(matches!(
            normalize_nickname("trailing."),
            Err(IdentityError::UnsafePathComponent)
        ));
    }
}
