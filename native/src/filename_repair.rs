//! Automatic filename targets; filesystem path identity remains canonical NFC.

use encoding_rs::{EUC_KR, WINDOWS_1252};
use std::collections::BTreeSet;

fn unsafe_name(name: &str) -> bool {
    name.chars()
        .any(|c| c.is_control() || matches!(c, '/' | '\\' | '\u{fffd}'))
}

fn windows_byte(c: char) -> Option<u8> {
    if c.is_ascii() || ('\u{a0}'..='\u{ff}').contains(&c) {
        return Some(c as u8);
    }
    const SPECIAL: &str = "€\u{81}‚ƒ„…†‡ˆ‰Š‹Œ\u{8d}Ž\u{8f}\u{90}‘’“”•–—˜™š›œ\u{9d}žŸ";
    SPECIAL
        .chars()
        .position(|value| value == c)
        .map(|index| 0x80 + index as u8)
}

fn single_byte(c: char) -> bool {
    (c as u32) <= 0xff || windows_byte(c).is_some()
}

fn korean_result(decoded: &str, minimum: usize) -> bool {
    let mut syllables = 0;
    for c in decoded.chars() {
        if ('가'..='힣').contains(&c) {
            syllables += 1;
        } else if !c.is_ascii() || c.is_control() || matches!(c, '/' | '\\') {
            return false;
        }
    }
    syllables >= minimum
}

fn known_korean_filename_word(decoded: &str) -> bool {
    // A byte roundtrip proves recoverability, not the user's intent. Legacy
    // encodings map ordinary Latin/symbol pairs to arbitrary Korean syllables.
    // Limit automatic legacy repair to recognizable filename vocabulary.
    const WORDS: &[&str] = &[
        "보고서",
        "계획서",
        "회의록",
        "최종",
        "포스터",
        "자료",
        "문서",
        "제안서",
        "발표",
        "사진",
        "이력서",
        "계약서",
        "신청서",
        "안내",
        "목록",
    ];
    WORDS.iter().any(|word| decoded.contains(word))
}

fn repair_span(span: &str) -> String {
    let non_ascii = span.chars().filter(|c| !c.is_ascii()).count();
    if non_ascii < 4 {
        return span.into();
    }
    let mut candidates = BTreeSet::new();
    let mut interpretations = BTreeSet::new();
    for windows in [false, true] {
        let bytes: Option<Vec<u8>> = span
            .chars()
            .map(|c| {
                if windows {
                    windows_byte(c)
                } else {
                    u8::try_from(c as u32).ok()
                }
            })
            .collect();
        let Some(bytes) = bytes else { continue };
        // Reverse interpretation must preserve the complete original span.
        let roundtrip = if windows {
            WINDOWS_1252
                .decode_without_bom_handling_and_without_replacement(&bytes)
                .map(|value| value.into_owned())
        } else {
            Some(bytes.iter().map(|&b| char::from(b)).collect())
        };
        if roundtrip.as_deref() != Some(span) {
            continue;
        }
        if let Ok(decoded) = std::str::from_utf8(&bytes) {
            interpretations.insert(decoded.to_owned());
            if korean_result(decoded, 2) {
                candidates.insert(decoded.to_owned());
            }
        }
        // Legacy byte pairs are easy to invent from ordinary Latin text.
        // Require a longer Korean result and visible non-letter byte artifacts.
        let artifacts = span.chars().any(|c| {
            matches!(
                c,
                '\u{a4}' | '\u{a6}' | '\u{a7}' | '\u{a8}' | '\u{aa}'
                    ..='\u{bf}' | '\u{d7}' | '\u{f7}'
            ) || (!c.is_ascii() && !c.is_alphabetic())
        });
        if let Some(decoded) = EUC_KR.decode_without_bom_handling_and_without_replacement(&bytes) {
            let (encoded, _, errors) = EUC_KR.encode(&decoded);
            if !errors && encoded.as_ref() == bytes {
                interpretations.insert(decoded.to_string());
                if non_ascii >= 6
                    && artifacts
                    && korean_result(&decoded, 3)
                    && known_korean_filename_word(&decoded)
                {
                    candidates.insert(decoded.into_owned());
                }
            }
        }
    }
    if candidates.len() == 1 && interpretations.len() == 1 {
        candidates.pop_first().unwrap()
    } else {
        span.into()
    }
}

pub fn filename_target(name: &str) -> String {
    if name.is_ascii() || unsafe_name(name) {
        return name.into();
    }
    // Filesystems may expose decomposed accents in the mojibake itself. NFC
    // restores those characters before the strictly reversible byte check.
    let canonical = crate::policy::nfc(name);
    let mut target = String::with_capacity(canonical.len());
    let mut start = 0;
    for (index, c) in canonical.char_indices() {
        if !single_byte(c) {
            target.push_str(&repair_span(&canonical[start..index]));
            target.push(c);
            start = index + c.len_utf8();
        }
    }
    target.push_str(&repair_span(&canonical[start..]));
    crate::policy::nfc(&target)
}

/// Directory identities can own exclusions and history paths. Encoding repair
/// therefore applies only to regular files; every other entry retains NFC.
pub fn entry_target(name: &str, regular_file: bool) -> String {
    if regular_file {
        filename_target(name)
    } else {
        crate::policy::nfc(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BROKEN: &str = "µµÀüÀÇ IRÆ÷½ºÅÍ-ÃÖÁ¾º».pdf";
    const FIXED: &str = "도전의 IR포스터-최종본.pdf";

    #[test]
    fn repairs_the_reversible_korean_legacy_sample() {
        assert_eq!(filename_target(BROKEN), FIXED);
        assert_eq!(filename_target(FIXED), FIXED);
    }

    #[test]
    fn repairs_utf8_mojibake_and_preserves_intact_parts() {
        assert_eq!(filename_target("í•œê¸€.txt"), "한글.txt");
        assert_eq!(
            filename_target(&format!("정상 👩🏽‍💻 {BROKEN}")),
            format!("정상 👩🏽‍💻 {FIXED}")
        );
        assert_eq!(filename_target("한글.txt"), "한글.txt");
    }

    #[test]
    fn leaves_normal_and_weak_names_unchanged() {
        for name in [
            "Résumé Noël ÅÄÖÜ.pdf",
            "mañana-über-façade.txt",
            "naïve — Côte d’Azur.txt",
            "©2026 €100 – report.pdf",
            "“report”book.txt",
            "한글 中文 日本語 👩🏽‍💻.txt",
            "µµ.txt",
            "ÇÑ.txt",
            "í•œ.txt",
            "plain ASCII (1)_1.xlsx",
        ] {
            assert_eq!(filename_target(name), name, "{name}");
        }
    }

    #[test]
    fn refuses_unsafe_or_incomplete_input() {
        for name in [
            "í•œê¸.txt",
            "bad\u{fffd}name.txt",
            "../µµÀüÀÇ.pdf",
            "bad\0name",
            "bad\nname",
            "í\u{95}\u{9c}ê¸\u{80}.txt",
        ] {
            assert_eq!(filename_target(name), name, "{name:?}");
        }
    }

    #[test]
    fn refuses_competing_lossless_utf8_and_legacy_interpretations() {
        // These bytes also decode losslessly as the CP949 text 怨듦났.
        // Choosing the Korean-looking interpretation would be a guess.
        assert_eq!(filename_target("ê³µê³µ.pdf"), "ê³µê³µ.pdf");
    }

    #[test]
    fn leaves_reversible_symbol_sequences_without_korean_word_evidence() {
        for name in ["°±²³´µ.txt", "ÀÁÂÃÄÅ °±.txt", "”ì”ì”ì.txt"] {
            assert_eq!(filename_target(name), name, "{name}");
        }
    }

    #[test]
    fn handles_extended_cp949_and_decomposed_mojibake_idempotently() {
        use unicode_normalization::UnicodeNormalization;
        assert_eq!(filename_target("”î”î”î ÀÚ·á.txt"), "뷁뷁뷁 자료.txt");
        assert_eq!(filename_target(&BROKEN.nfd().collect::<String>()), FIXED);
        for name in [BROKEN, FIXED, "í•œê¸€.txt", "정상 ”ì”ì”ì.txt", "Résumé.txt"]
        {
            let target = filename_target(name);
            assert_eq!(filename_target(&target), target);
        }
    }
}
