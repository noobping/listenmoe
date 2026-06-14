#[cfg(any(target_os = "windows", test))]
#[cfg_attr(test, allow(dead_code))]
mod embedded {
    use std::{collections::HashMap, env, sync::OnceLock};

    const DE_PO: &str = include_str!("../po/de.po");
    const ES_PO: &str = include_str!("../po/es.po");
    const JA_PO: &str = include_str!("../po/ja.po");
    const NL_PO: &str = include_str!("../po/nl.po");

    static ACTIVE_LANGUAGE: OnceLock<Language> = OnceLock::new();
    static DE_TRANSLATIONS: OnceLock<HashMap<String, String>> = OnceLock::new();
    static ES_TRANSLATIONS: OnceLock<HashMap<String, String>> = OnceLock::new();
    static JA_TRANSLATIONS: OnceLock<HashMap<String, String>> = OnceLock::new();
    static NL_TRANSLATIONS: OnceLock<HashMap<String, String>> = OnceLock::new();

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Language {
        English,
        German,
        Spanish,
        Japanese,
        Dutch,
    }

    pub fn init_i18n() {
        let language = active_language();
        let _ = translations_for(language);

        if crate::log::is_verbose() {
            println!("Using embedded Windows translations: {language:?}");
        }
    }

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
            .unwrap_or(Language::English)
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
        let lang_id = unsafe { get_user_default_ui_language() };
        language_from_windows_lang_id(lang_id)
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
    }

    fn language_from_locale(locale: &str) -> Option<Language> {
        let locale = locale.trim();
        if locale.eq_ignore_ascii_case("c") || locale.eq_ignore_ascii_case("posix") {
            return Some(Language::English);
        }

        let normalized = locale
            .split('.')
            .next()
            .unwrap_or(locale)
            .split('@')
            .next()
            .unwrap_or(locale)
            .to_ascii_lowercase();
        let language = normalized
            .split(['_', '-'])
            .next()
            .unwrap_or(normalized.as_str());

        match language {
            "c" | "posix" => Some(Language::English),
            "de" => Some(Language::German),
            "en" => Some(Language::English),
            "es" => Some(Language::Spanish),
            "ja" => Some(Language::Japanese),
            "nl" => Some(Language::Dutch),
            _ => None,
        }
    }

    fn language_from_windows_lang_id(lang_id: u16) -> Option<Language> {
        match lang_id & 0x03ff {
            0x07 => Some(Language::German),
            0x09 => Some(Language::English),
            0x0a => Some(Language::Spanish),
            0x11 => Some(Language::Japanese),
            0x13 => Some(Language::Dutch),
            _ => None,
        }
    }

    fn translations_for(language: Language) -> Option<&'static HashMap<String, String>> {
        match language {
            Language::English => None,
            Language::German => Some(DE_TRANSLATIONS.get_or_init(|| parse_po(DE_PO))),
            Language::Spanish => Some(ES_TRANSLATIONS.get_or_init(|| parse_po(ES_PO))),
            Language::Japanese => Some(JA_TRANSLATIONS.get_or_init(|| parse_po(JA_PO))),
            Language::Dutch => Some(NL_TRANSLATIONS.get_or_init(|| parse_po(NL_PO))),
        }
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
            assert_eq!(language_from_locale("nl_NL.UTF-8"), Some(Language::Dutch));
            assert_eq!(language_from_locale("de-DE"), Some(Language::German));
            assert_eq!(language_from_locale("es_ES"), Some(Language::Spanish));
            assert_eq!(language_from_locale("ja_JP"), Some(Language::Japanese));
            assert_eq!(language_from_locale("C"), Some(Language::English));
            assert_eq!(language_from_locale("C.UTF-8"), Some(Language::English));
            assert_eq!(language_from_locale("POSIX"), Some(Language::English));
            assert_eq!(language_from_locale("POSIX.UTF-8"), Some(Language::English));
            assert_eq!(language_from_locale("fr_FR.UTF-8"), None);
        }

        #[test]
        fn maps_windows_primary_language_ids() {
            assert_eq!(
                language_from_windows_lang_id(0x0407),
                Some(Language::German)
            );
            assert_eq!(
                language_from_windows_lang_id(0x0409),
                Some(Language::English)
            );
            assert_eq!(
                language_from_windows_lang_id(0x0c0a),
                Some(Language::Spanish)
            );
            assert_eq!(
                language_from_windows_lang_id(0x0411),
                Some(Language::Japanese)
            );
            assert_eq!(language_from_windows_lang_id(0x0413), Some(Language::Dutch));
            assert_eq!(language_from_windows_lang_id(0x040c), None);
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod native {
    use dirs_next as dirs;
    use gettextrs::{
        bind_textdomain_codeset, bindtextdomain, gettext as native_gettext, setlocale, textdomain,
        LocaleCategory,
    };
    use std::{
        env,
        path::{Path, PathBuf},
    };

    const APP_ID: &str = "io.github.noobping.listenmoe";

    fn find_locale_dir() -> PathBuf {
        // Developer directory (cargo run)
        let dev_dir = Path::new("data").join("locale");
        if dev_dir.is_dir() {
            return dev_dir;
        }

        // AppImage
        if let Ok(appdir) = env::var("APPDIR") {
            let candidate = Path::new(&appdir).join("usr").join("share").join("locale");
            if candidate.is_dir() {
                return candidate;
            }
        }

        // exe dir
        if let Ok(exe) = env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                let candidate = exe_dir.join("locale");
                if candidate.is_dir() {
                    return candidate;
                }
            }
        }

        // Flatpak
        let app_share_locale = Path::new("/app/share/locale");
        if app_share_locale.is_dir() {
            return app_share_locale.to_path_buf();
        }

        // User-level data dir
        if let Some(base) = dirs::data_local_dir() {
            let candidate = base.join(APP_ID).join("locale");
            if candidate.is_dir() {
                return candidate;
            }
        }

        // System locale directory
        let sys_dir = Path::new("/usr/share/locale");
        if sys_dir.is_dir() {
            return sys_dir.to_path_buf();
        }

        // Fallback
        dev_dir.to_path_buf()
    }

    pub fn init_i18n() {
        setlocale(LocaleCategory::LcAll, "");

        let dir = find_locale_dir();
        if crate::log::is_verbose() {
            println!("Using locale dir: {}", dir.display());
        }

        let dir_str = dir.to_str().expect("Locale path must be UTF-8 for gettext");

        bindtextdomain(APP_ID, dir_str).expect("bindtextdomain failed");
        bind_textdomain_codeset(APP_ID, "UTF-8").expect("bind codeset failed");
        textdomain(APP_ID).expect("textdomain failed");
    }

    pub fn gettext(message: &str) -> String {
        native_gettext(message)
    }
}

#[cfg(target_os = "windows")]
pub use embedded::{gettext, init_i18n};

#[cfg(not(target_os = "windows"))]
pub use native::{gettext, init_i18n};
