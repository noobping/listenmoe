#[cfg(any(target_os = "windows", test))]
#[cfg_attr(test, allow(dead_code))]
mod embedded;

#[cfg(not(target_os = "windows"))]
mod native;

#[cfg(target_os = "windows")]
pub use embedded::{gettext, init_i18n};

#[cfg(not(target_os = "windows"))]
pub use native::{gettext, init_i18n};
