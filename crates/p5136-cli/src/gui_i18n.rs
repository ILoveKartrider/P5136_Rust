use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) enum GuiLanguage {
    #[default]
    Korean,
    English,
    SimplifiedChinese,
}

impl GuiLanguage {
    pub(crate) const ALL: [Self; 3] = [Self::Korean, Self::English, Self::SimplifiedChinese];

    pub(crate) const fn native_label(self) -> &'static str {
        match self {
            Self::Korean => "한국어",
            Self::English => "English",
            Self::SimplifiedChinese => "简体中文",
        }
    }
}

pub(crate) const fn pick<'a>(
    language: GuiLanguage,
    korean: &'a str,
    english: &'a str,
    simplified_chinese: &'a str,
) -> &'a str {
    match language {
        GuiLanguage::Korean => korean,
        GuiLanguage::English => english,
        GuiLanguage::SimplifiedChinese => simplified_chinese,
    }
}

macro_rules! tr {
    ($language:expr, $korean:literal, $english:literal, $simplified_chinese:literal) => {
        $crate::gui_i18n::pick($language, $korean, $english, $simplified_chinese)
    };
}

macro_rules! tr_format {
    ($language:expr, $korean:literal, $english:literal, $simplified_chinese:literal $(, $argument:expr)* $(,)?) => {{
        match $language {
            $crate::gui_i18n::GuiLanguage::Korean => format!($korean $(, $argument)*),
            $crate::gui_i18n::GuiLanguage::English => format!($english $(, $argument)*),
            $crate::gui_i18n::GuiLanguage::SimplifiedChinese => {
                format!($simplified_chinese $(, $argument)*)
            }
        }
    }};
}

pub(crate) use {tr, tr_format};

#[cfg(test)]
mod tests {
    use super::{GuiLanguage, pick};

    #[test]
    fn all_languages_have_stable_native_labels_and_selection() {
        assert_eq!(GuiLanguage::ALL.len(), 3);
        assert_eq!(GuiLanguage::Korean.native_label(), "한국어");
        assert_eq!(GuiLanguage::English.native_label(), "English");
        assert_eq!(GuiLanguage::SimplifiedChinese.native_label(), "简体中文");
        assert_eq!(pick(GuiLanguage::Korean, "한", "en", "中"), "한");
        assert_eq!(pick(GuiLanguage::English, "한", "en", "中"), "en");
        assert_eq!(pick(GuiLanguage::SimplifiedChinese, "한", "en", "中"), "中");
    }
}
