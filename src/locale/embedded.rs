use std::{collections::HashMap, env, sync::OnceLock};

// Keep this list in sync with po/LINGUAS; the catalog coverage test checks it.
struct Catalog {
    locale: &'static str,
    source: &'static str,
    translations: OnceLock<HashMap<String, String>>,
}

macro_rules! catalogs {
    ($($locale:literal),+ $(,)?) => {
        static CATALOGS: [Catalog; [$($locale),+].len()] = [
            $(Catalog {
                locale: $locale,
                source: include_str!(concat!("../../po/", $locale, ".po")),
                translations: OnceLock::new(),
            }),+
        ];
    };
}

catalogs!(
    "ar", "az", "be", "bg", "bn", "br", "ca", "ckb", "cs", "da", "de", "el", "en", "en_GB", "eo",
    "es", "et", "eu", "fa", "fi", "fil", "fr", "ga", "gl", "he", "hi", "hr", "hu", "hy", "ia",
    "id", "it", "ja", "kab", "ko", "kw", "lt", "nb", "nl", "oc", "pa", "pl", "pt", "pt_BR", "ro",
    "ru", "si", "sk", "sq", "sv", "ta", "tr", "uk", "vi", "zh_CN", "zh_TW",
);

type Language = &'static str;
static ACTIVE_LANGUAGE: OnceLock<Language> = OnceLock::new();

pub fn init_i18n() {
    let language = active_language();
    let _ = translations_for(language);

    if crate::log::is_verbose() {
        println!("Using embedded Windows translations: {language:?}");
    }
}

pub fn init_process_locale() {}

pub fn gettext(message: &str) -> String {
    translate(translations_for(active_language()), message)
}

fn translate(translations: Option<&HashMap<String, String>>, message: &str) -> String {
    let Some(translations) = translations else {
        return message.to_string();
    };

    translations
        .get(message)
        .filter(|translation| !translation.is_empty())
        .cloned()
        .unwrap_or_else(|| message.to_string())
}

fn active_language() -> Language {
    *ACTIVE_LANGUAGE.get_or_init(detect_language)
}

fn detect_language() -> Language {
    detect_environment_language()
        .or_else(detect_platform_language)
        .unwrap_or("en")
}

fn detect_environment_language() -> Option<Language> {
    ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|key| env::var(key).ok())
        .flat_map(|value| {
            value
                .split(':')
                .map(str::trim)
                .filter(|locale| !locale.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .find_map(|locale| language_from_locale(&locale))
}

#[cfg(target_os = "windows")]
fn detect_platform_language() -> Option<Language> {
    // Resolve the UI language ID through Windows so regional/script variants
    // survive (notably pt-BR, en-GB, and Traditional Chinese).
    let mut name = [0u16; 85]; // LOCALE_NAME_MAX_LENGTH, including the terminator.
    let length = unsafe {
        locale_id_to_name(
            u32::from(get_user_default_ui_language()),
            name.as_mut_ptr(),
            name.len() as i32,
            0,
        )
    };
    if length <= 1 {
        return None;
    }
    language_from_locale(&String::from_utf16_lossy(&name[..length as usize - 1]))
}

#[cfg(not(target_os = "windows"))]
fn detect_platform_language() -> Option<Language> {
    None
}

#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "GetUserDefaultUILanguage"]
    fn get_user_default_ui_language() -> u16;
    #[link_name = "LCIDToLocaleName"]
    fn locale_id_to_name(locale: u32, name: *mut u16, length: i32, flags: u32) -> i32;
}

fn language_from_locale(locale: &str) -> Option<Language> {
    let normalized = locale
        .trim()
        .split(['.', '@'])
        .next()
        .unwrap_or("")
        .replace('-', "_")
        .to_ascii_lowercase();
    let parts: Vec<_> = normalized.split('_').collect();
    let language = parts[0];
    match language {
        "c" | "posix" => return Some("en"),
        "zh" => {
            // Explicit script takes precedence over territory, e.g. zh-Hans-HK.
            return Some(if parts.contains(&"hans") {
                "zh_CN"
            } else if parts
                .iter()
                .any(|part| ["hant", "tw", "hk", "mo"].contains(part))
            {
                "zh_TW"
            } else {
                "zh_CN"
            });
        }
        "tl" => return Some("fil"),
        "iw" => return Some("he"),
        "no" => return Some("nb"),
        _ => {}
    }

    CATALOGS
        .iter()
        .find(|catalog| catalog.locale.eq_ignore_ascii_case(&normalized))
        .or_else(|| CATALOGS.iter().find(|catalog| catalog.locale == language))
        .map(|catalog| catalog.locale)
}

fn translations_for(language: Language) -> Option<&'static HashMap<String, String>> {
    CATALOGS
        .iter()
        .find(|catalog| catalog.locale == language)
        .map(|catalog| {
            catalog
                .translations
                .get_or_init(|| parse_po(catalog.source))
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PoField {
    MsgId,
    MsgStr,
}

fn parse_po(source: &str) -> HashMap<String, String> {
    let mut translations = HashMap::new();
    let mut msgid = String::new();
    let mut msgstr = String::new();
    let mut current_field = None;

    for line in source.lines() {
        let line = line.trim();

        if line.is_empty() {
            insert_translation(&mut translations, &mut msgid, &mut msgstr);
            current_field = None;
            continue;
        }

        if line.starts_with('#') {
            continue;
        }

        if let Some(value) = line.strip_prefix("msgid ") {
            insert_translation(&mut translations, &mut msgid, &mut msgstr);
            msgid = parse_po_string(value);
            msgstr.clear();
            current_field = Some(PoField::MsgId);
            continue;
        }

        if let Some(value) = line.strip_prefix("msgstr ") {
            msgstr = parse_po_string(value);
            current_field = Some(PoField::MsgStr);
            continue;
        }

        if line.starts_with('"') {
            match current_field {
                Some(PoField::MsgId) => msgid.push_str(&parse_po_string(line)),
                Some(PoField::MsgStr) => msgstr.push_str(&parse_po_string(line)),
                None => {}
            }
        }
    }

    insert_translation(&mut translations, &mut msgid, &mut msgstr);
    translations
}

fn insert_translation(
    translations: &mut HashMap<String, String>,
    msgid: &mut String,
    msgstr: &mut String,
) {
    if !msgid.is_empty() && !msgstr.is_empty() {
        translations.insert(std::mem::take(msgid), std::mem::take(msgstr));
    } else {
        msgid.clear();
        msgstr.clear();
    }
}

fn parse_po_string(value: &str) -> String {
    let value = value.trim();
    let quoted = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value);

    unescape_po_string(quoted)
}

fn unescape_po_string(value: &str) -> String {
    let mut unescaped = String::with_capacity(value.len());
    let mut chars = value.chars();

    while let Some(ch) = chars.next() {
        if ch != '\\' {
            unescaped.push(ch);
            continue;
        }

        match chars.next() {
            Some('n') => unescaped.push('\n'),
            Some('r') => unescaped.push('\r'),
            Some('t') => unescaped.push('\t'),
            Some('"') => unescaped.push('"'),
            Some('\\') => unescaped.push('\\'),
            Some(other) => {
                unescaped.push('\\');
                unescaped.push(other);
            }
            None => unescaped.push('\\'),
        }
    }

    unescaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_entries() {
        let translations = parse_po(
            r#"
msgid "Preferences"
msgstr "Voorkeuren"
"#,
        );

        assert_eq!(
            translations.get("Preferences").map(String::as_str),
            Some("Voorkeuren")
        );
    }

    #[test]
    fn parses_multiline_entries() {
        let translations = parse_po(
            r#"
msgid ""
"Try again "
"later."
msgstr ""
"Probeer het "
"later opnieuw."
"#,
        );

        assert_eq!(
            translations.get("Try again later.").map(String::as_str),
            Some("Probeer het later opnieuw.")
        );
    }

    #[test]
    fn parses_common_escapes() {
        let translations = parse_po(
            r#"
msgid "Line\n\"quoted\"\\tab\tend"
msgstr "Regel\n\"geciteerd\"\\tab\tklaar"
"#,
        );

        assert_eq!(
            translations
                .get("Line\n\"quoted\"\\tab\tend")
                .map(String::as_str),
            Some("Regel\n\"geciteerd\"\\tab\tklaar")
        );
    }

    #[test]
    fn skips_empty_translations() {
        let translations = parse_po(
            r#"
msgid "Missing"
msgstr ""
"#,
        );

        assert!(!translations.contains_key("Missing"));
    }

    #[test]
    fn falls_back_for_missing_and_empty_translations() {
        let mut translations = HashMap::new();
        translations.insert("Known".to_string(), "Bekend".to_string());
        translations.insert("Empty".to_string(), String::new());

        assert_eq!(translate(Some(&translations), "Known"), "Bekend");
        assert_eq!(translate(Some(&translations), "Missing"), "Missing");
        assert_eq!(translate(Some(&translations), "Empty"), "Empty");
        assert_eq!(translate(None, "English"), "English");
    }

    #[test]
    fn detects_language_from_locale_strings() {
        for (locale, expected) in [
            ("nl_NL.UTF-8", "nl"),
            ("de-DE", "de"),
            ("es_ES", "es"),
            ("ja_JP", "ja"),
            ("fr_FR.UTF-8", "fr"),
            ("C", "en"),
            ("C.UTF-8", "en"),
            ("POSIX", "en"),
            ("POSIX.UTF-8", "en"),
            (" en-GB ", "en_GB"),
            ("en_AU", "en"),
            ("pt-BR", "pt_BR"),
            ("pt_PT.UTF-8", "pt"),
            ("ca_ES@valencia", "ca"),
            ("nb-NO", "nb"),
            ("no_NO", "nb"),
            ("fil-PH", "fil"),
            ("tl_PH", "fil"),
            ("iw_IL", "he"),
            ("ckb_IQ", "ckb"),
            ("zh-CN", "zh_CN"),
            ("zh-SG", "zh_CN"),
            ("zh-Hans", "zh_CN"),
            ("zh-Hans-HK", "zh_CN"),
            ("zh-Hant", "zh_TW"),
            ("zh-TW", "zh_TW"),
            ("zh_HK.UTF-8", "zh_TW"),
            ("zh-MO", "zh_TW"),
        ] {
            assert_eq!(language_from_locale(locale), Some(expected), "{locale}");
        }
        assert_eq!(language_from_locale("zz_ZZ"), None);
        assert_eq!(language_from_locale(""), None);
    }

    #[test]
    fn all_packaged_catalogs_are_embedded_and_complete() {
        let locales: Vec<_> = include_str!("../../po/LINGUAS")
            .split_whitespace()
            .collect();
        let expected = parse_po(include_str!("../../po/en.po"));
        assert_eq!(CATALOGS.len(), locales.len());
        for locale in locales {
            assert_eq!(language_from_locale(locale), Some(locale));
            let translations = translations_for(locale).expect("catalog must be embedded");
            assert_eq!(translations.len(), expected.len(), "{locale}");
            for message in expected.keys() {
                let translation = translations.get(message).expect("missing message");
                assert!(!translation.trim().is_empty(), "{locale}: {message}");
                assert_eq!(
                    message.matches("%s").count(),
                    translation.matches("%s").count(),
                    "{locale}: {message}"
                );
            }
        }
    }

    #[test]
    fn uses_regional_catalogs_and_preserves_fallback() {
        assert_eq!(
            translate(translations_for("pt_BR"), "Downloaded"),
            "Baixado"
        );
        assert_eq!(
            translate(translations_for("pt"), "Downloaded"),
            "Transferido"
        );
        assert_eq!(
            translate(translations_for("en_GB"), "Update canceled"),
            "Update cancelled"
        );
        assert_eq!(
            translate(translations_for("zh_CN"), "Preferences"),
            "偏好设置"
        );
        assert_eq!(
            translate(translations_for("zh_TW"), "Preferences"),
            "偏好設定"
        );
        assert_eq!(
            translate(translations_for("fr"), "Unknown message"),
            "Unknown message"
        );
    }
}
