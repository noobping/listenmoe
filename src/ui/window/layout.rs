use crate::locale::gettext;
use crate::ui::ScrollingWindowTitle as WindowTitle;
use adw::{
    gtk::{
        self, gio::Menu, prelude::WidgetExt, ApplicationWindow, Button, GestureClick, HeaderBar,
        MenuButton, Orientation, Picture, Popover,
    },
    prelude::*,
    Application, StyleManager,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

#[cfg(target_os = "linux")]
use super::super::volume;
use super::super::{cover, karaoke, progress, viz};
use super::state::{SharedFlag, SharedTitle};
use super::APP_NAME;

const APP_ID: &str = "io.github.noobping.listenmoe";

const HEADER_HEIGHT: i32 = 50;
const N_VIZ_BARS: usize = 48;

pub(super) struct WindowLayout {
    pub(super) window: ApplicationWindow,
    pub(super) win_title: WindowTitle,
    pub(super) normal_title: SharedTitle,
    pub(super) playback_playing: SharedFlag,
    pub(super) update_active: SharedFlag,
    pub(super) update_title_override: SharedFlag,
    pub(super) play_button: Button,
    pub(super) pause_button: Button,
    #[cfg(target_os = "linux")]
    pub(super) volume_button: gtk::ScaleButton,
    pub(super) menu: Menu,
    pub(super) art_picture: Picture,
    pub(super) art_popover: Popover,
    pub(super) art_stack: gtk::Stack,
    pub(super) karaoke_view: karaoke::KaraokeView,
    pub(super) style_manager: StyleManager,
    pub(super) css_provider: gtk::CssProvider,
    pub(super) viz: gtk::DrawingArea,
    pub(super) viz_handle: viz::VizHandle,
    pub(super) titlebar_progress: progress::TitlebarProgress,
}

pub(super) fn build_window_layout(app: &Application, pause_resume_enabled: bool) -> WindowLayout {
    let default_subtitle = gettext("J-POP and K-POP radio");
    let normal_title = Rc::new(RefCell::new((
        APP_NAME.to_string(),
        default_subtitle.clone(),
    )));
    let playback_playing = Rc::new(Cell::new(false));
    let update_active = Rc::new(Cell::new(false));
    let update_title_override = Rc::new(Cell::new(false));

    let win_title = WindowTitle::new(APP_NAME, &default_subtitle);

    let play_button = Button::from_icon_name("media-playback-start-symbolic");
    play_button.set_action_name(Some("win.play"));

    let pause_button_icon = if pause_resume_enabled {
        "media-playback-pause-symbolic"
    } else {
        "media-playback-stop-symbolic"
    };
    let pause_button = Button::from_icon_name(pause_button_icon);
    pause_button.set_action_name(Some("win.pause"));
    pause_button.set_visible(false);

    #[cfg(target_os = "linux")]
    let volume_button = volume::build_button();

    let window = ApplicationWindow::builder()
        .application(app)
        .title(APP_NAME)
        .icon_name(APP_ID)
        .default_width(300)
        .default_height(HEADER_HEIGHT)
        .resizable(false)
        .build();
    window.add_css_class("cover-tint");

    let style_manager = StyleManager::default();
    style_manager.set_color_scheme(adw::ColorScheme::Default);
    let css_provider = cover::install_css_provider();

    let menu = Menu::new();
    let more_button = MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text(gettext("Main Menu"))
        .menu_model(&menu)
        .build();

    let buttons = gtk::Box::new(Orientation::Horizontal, 0);
    buttons.append(&more_button);
    buttons.append(&play_button);
    buttons.append(&pause_button);
    #[cfg(target_os = "linux")]
    buttons.append(&volume_button);

    let close_btn = Button::from_icon_name("window-close-symbolic");
    close_btn.set_action_name(Some("win.quit"));

    let title_area = gtk::CenterBox::new();
    title_area.set_hexpand(true);
    title_area.set_center_widget(Some(&win_title));

    let title_row = gtk::Box::new(Orientation::Horizontal, 0);
    title_row.set_hexpand(true);
    title_row.append(&buttons);
    title_row.append(&title_area);
    title_row.append(&close_btn);

    let header = HeaderBar::new();
    header.set_title_widget(Some(&title_row));
    header.set_show_title_buttons(false);
    header.add_css_class("cover-tint");
    header.set_height_request(HEADER_HEIGHT);

    let art_picture = Picture::builder()
        .can_shrink(true)
        .focusable(false)
        .sensitive(false)
        .build();
    let karaoke_view = karaoke::KaraokeView::new();
    let art_stack = gtk::Stack::new();
    art_stack.set_hhomogeneous(true);
    art_stack.set_vhomogeneous(true);
    art_stack.set_transition_duration(190);
    art_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    art_stack.add_named(&art_picture, Some("art"));
    art_stack.add_named(karaoke_view.widget(), Some("karaoke"));
    art_stack.set_visible_child_name("art");

    let art_popover = Popover::builder()
        .has_arrow(true)
        .position(gtk::PositionType::Bottom)
        .autohide(true)
        .child(&art_stack)
        .build();
    art_popover.set_parent(&header);
    art_popover.add_css_class("cover-tint");

    let title_click = GestureClick::new();
    {
        let picture = art_picture.clone();
        let art = art_popover.clone();
        let stack = art_stack.clone();
        title_click.connect_released(move |_, _, _, _| {
            stack.set_visible_child_name("art");
            if art.is_visible() {
                art.popdown();
            } else if picture.paintable().is_some() {
                art.popup();
            }
        });
    }
    win_title.add_controller(title_click);

    let close_any_click = GestureClick::new();
    {
        let art = art_popover.clone();
        close_any_click.connect_released(move |_, _, _, _| {
            art.popdown();
        });
    }
    art_popover.add_controller(close_any_click);

    let overlay = gtk::Overlay::new();
    overlay.add_css_class("titlebar-tint");
    overlay.set_height_request(HEADER_HEIGHT);

    let (viz, viz_handle) = viz::make_bars_visualizer(N_VIZ_BARS, HEADER_HEIGHT);
    overlay.set_child(Some(&viz));

    let (titlebar_progress_area, titlebar_progress) = progress::make_titlebar_progress();

    header.add_css_class("viz-transparent");
    header.add_css_class("cover-tint");
    overlay.add_overlay(&header);
    overlay.add_overlay(&titlebar_progress_area);
    window.set_titlebar(Some(&overlay));

    let dummy = gtk::Box::new(Orientation::Vertical, 0);
    dummy.set_height_request(0);
    dummy.set_vexpand(false);
    window.set_child(Some(&dummy));

    WindowLayout {
        window,
        win_title,
        normal_title,
        playback_playing,
        update_active,
        update_title_override,
        play_button,
        pause_button,
        #[cfg(target_os = "linux")]
        volume_button,
        menu,
        art_picture,
        art_popover,
        art_stack,
        karaoke_view,
        style_manager,
        css_provider,
        viz,
        viz_handle,
        titlebar_progress,
    }
}
