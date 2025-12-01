use gettextrs::{
    bind_textdomain_codeset, bindtextdomain, setlocale, textdomain, LocaleCategory,
};
use std::{env, fs, path::Path};

const APP_ID: &str = env!("APP_ID");

pub fn init_i18n() {
    setlocale(LocaleCategory::LcAll, "");

    // dev dir
    let dir = Path::new("data").join("locale");
    fs::create_dir_all(&dir).unwrap();

    // TODO: prod needs to be to /usr/share/locale

    bindtextdomain(APP_ID, dir).expect("bindtextdomain failed");
    bind_textdomain_codeset(APP_ID, "UTF-8").expect("bind codeset failed");
    textdomain(APP_ID).expect("textdomain failed");
}

#[macro_export]
macro_rules! t {
    ($msg:literal) => {
        gettextrs::gettext($msg)
    };
}
