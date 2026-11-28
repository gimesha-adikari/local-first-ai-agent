//! Shared settings of the visual input workflows (Stage 04-E): screen
//! regions, camera snapshots and attached images use the same OCR language
//! choice and remember the last translation target. Nothing here is image
//! content, recognized text or a query.

use serde::{Deserialize, Serialize};

use crate::inference::{ErrorCode, InferenceError, Result};

/// Most recognition languages chosen at once.
pub const MAX_OCR_LANGUAGES: usize = 4;
/// Longest translation target name, in UTF-8 bytes.
pub const MAX_TARGET_BYTES: usize = 100;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisualSettings {
    /// Tesseract language data names, in order. Empty means the defaults
    /// derived from the system locale.
    #[serde(default)]
    pub ocr_languages: Vec<String>,
    /// The language translations were last requested in, for example
    /// "French". Empty means none chosen yet.
    #[serde(default)]
    pub translation_target: String,
}

/// A Tesseract language data name such as `eng` or `chi_sim`.
pub fn valid_ocr_language(name: &str) -> bool {
    let mut parts = name.split('_');
    parts.next().is_some_and(|code| {
        (3..=4).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_lowercase())
    }) && parts
        .all(|part| (2..=8).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_lowercase()))
        && name.len() <= 24
}

impl VisualSettings {
    pub fn validate(&self) -> Result<()> {
        let invalid = |message: &str| Err(InferenceError::new(ErrorCode::InvalidConfig, message));
        if self.ocr_languages.len() > MAX_OCR_LANGUAGES {
            return invalid("Choose up to four recognition languages.");
        }
        if !self
            .ocr_languages
            .iter()
            .all(|name| valid_ocr_language(name))
        {
            return invalid("A recognition language name is invalid.");
        }
        let mut seen = self.ocr_languages.clone();
        seen.sort();
        seen.dedup();
        if seen.len() != self.ocr_languages.len() {
            return invalid("Each recognition language can be chosen once.");
        }
        let target = self.translation_target.trim();
        if target.len() > MAX_TARGET_BYTES || target.chars().any(char::is_control) {
            return invalid(
                "Use a language name of up to 100 UTF-8 bytes without control characters.",
            );
        }
        Ok(())
    }

    /// The languages to recognize: the saved choice limited to installed
    /// data, or the locale defaults when nothing usable is saved.
    pub fn effective_languages(&self, installed: &[String], defaults: &[String]) -> Vec<String> {
        let chosen: Vec<String> = self
            .ocr_languages
            .iter()
            .filter(|name| installed.iter().any(|have| have == *name))
            .cloned()
            .collect();
        if chosen.is_empty() {
            defaults.to_vec()
        } else {
            chosen
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn settings_validate_and_fall_back_to_installed_defaults() {
        let mut settings = VisualSettings {
            ocr_languages: names(&["eng", "chi_sim", "ara"]),
            translation_target: "Français".into(),
        };
        settings.validate().unwrap();
        settings.ocr_languages.push("deu".into());
        settings.validate().unwrap();
        settings.ocr_languages.push("spa".into());
        assert!(settings.validate().is_err());
        for bad in ["ENG", "e", "eng/../x", "eng_", ""] {
            assert!(!valid_ocr_language(bad), "{bad}");
            assert!(
                VisualSettings {
                    ocr_languages: vec![bad.into()],
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            VisualSettings {
                ocr_languages: names(&["eng", "eng"]),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            VisualSettings {
                translation_target: "x\u{7}".into(),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        let installed = names(&["eng", "ara"]);
        let saved = VisualSettings {
            ocr_languages: names(&["ara", "jpn"]),
            ..Default::default()
        };
        assert_eq!(
            saved.effective_languages(&installed, &names(&["eng"])),
            names(&["ara"])
        );
        // Nothing usable saved: the locale defaults, unchanged.
        assert_eq!(
            VisualSettings::default().effective_languages(&installed, &names(&["eng"])),
            names(&["eng"])
        );
        let missing = VisualSettings {
            ocr_languages: names(&["jpn"]),
            ..Default::default()
        };
        assert_eq!(
            missing.effective_languages(&installed, &names(&["eng"])),
            names(&["eng"])
        );
    }
}
