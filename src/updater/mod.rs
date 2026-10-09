use crate::ui::{ScrollingWindowTitle as WindowTitle, TitlebarProgress};
use adw::gtk::Button;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

#[derive(Clone)]
pub(crate) struct UpdateUi {
    pub(crate) win_title: WindowTitle,
    pub(crate) normal_title: Rc<RefCell<(String, String)>>,
    pub(crate) playback_playing: Rc<Cell<bool>>,
    pub(crate) update_active: Rc<Cell<bool>>,
    pub(crate) update_title_override: Rc<Cell<bool>>,
    pub(crate) play_button: Button,
    pub(crate) pause_button: Button,
    pub(crate) titlebar_progress: TitlebarProgress,
}

mod common;
mod logic;
mod windows;

pub(crate) use common::{handle_special_command, register_window, UpdaterController};
