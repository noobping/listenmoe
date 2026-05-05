use adw::{
    gtk::{Button, DrawingArea},
    WindowTitle,
};
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
    pub(crate) update_button: Button,
    pub(crate) update_progress_area: DrawingArea,
    pub(crate) update_progress: Rc<Cell<Option<f64>>>,
}

mod common;
mod logic;
mod windows;

pub(crate) use common::{handle_special_command, register_window, UpdaterController};
