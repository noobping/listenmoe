use adw::{
    gtk::{
        self, gio::Menu, prelude::WidgetExt, ApplicationWindow, Button, GestureClick, HeaderBar,
        MenuButton, Orientation, Picture, Popover,
    },
    prelude::*,
    Application, StyleManager, WindowTitle,
};
use gettextrs::gettext;
#[cfg(target_os = "windows")]
use std::f64::consts::{FRAC_PI_2, TAU};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use super::super::{cover, viz};
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
    #[cfg(target_os = "windows")]
    pub(super) update_button: Button,
    #[cfg(target_os = "windows")]
    pub(super) update_progress_area: gtk::DrawingArea,
    #[cfg(target_os = "windows")]
    pub(super) update_progress: Rc<Cell<Option<f64>>>,
    pub(super) menu: Menu,
    pub(super) art_picture: Picture,
    pub(super) art_popover: Popover,
    pub(super) style_manager: StyleManager,
    pub(super) css_provider: gtk::CssProvider,
    pub(super) viz: gtk::DrawingArea,
    pub(super) viz_handle: viz::VizHandle,
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

    #[cfg(target_os = "windows")]
    let (update_button, update_progress_area, update_progress) = build_update_progress_button();

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
    #[cfg(target_os = "windows")]
    buttons.append(&update_button);

    let header = HeaderBar::new();
    header.pack_start(&buttons);
    header.set_title_widget(Some(&win_title));
    header.set_show_title_buttons(false);
    header.add_css_class("cover-tint");
    header.set_height_request(HEADER_HEIGHT);

    let art_picture = Picture::builder()
        .can_shrink(true)
        .focusable(false)
        .sensitive(false)
        .build();
    let art_popover = Popover::builder()
        .has_arrow(true)
        .position(gtk::PositionType::Bottom)
        .autohide(true)
        .child(&art_picture)
        .build();
    art_popover.set_parent(&header);
    art_popover.add_css_class("cover-tint");

    let title_click = GestureClick::new();
    {
        let picture = art_picture.clone();
        let art = art_popover.clone();
        title_click.connect_released(move |_, _, _, _| {
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

    let close_btn = Button::from_icon_name("window-close-symbolic");
    close_btn.set_action_name(Some("win.quit"));
    header.pack_end(&close_btn);

    let overlay = gtk::Overlay::new();
    overlay.add_css_class("titlebar-tint");
    overlay.set_height_request(HEADER_HEIGHT);

    let (viz, viz_handle) = viz::make_bars_visualizer(N_VIZ_BARS, HEADER_HEIGHT);
    overlay.set_child(Some(&viz));

    header.add_css_class("viz-transparent");
    header.add_css_class("cover-tint");
    overlay.add_overlay(&header);
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
        #[cfg(target_os = "windows")]
        update_button,
        #[cfg(target_os = "windows")]
        update_progress_area,
        #[cfg(target_os = "windows")]
        update_progress,
        menu,
        art_picture,
        art_popover,
        style_manager,
        css_provider,
        viz,
        viz_handle,
    }
}

#[cfg(target_os = "windows")]
fn build_update_progress_button() -> (Button, gtk::DrawingArea, Rc<Cell<Option<f64>>>) {
    let progress: Rc<Cell<Option<f64>>> = Rc::new(Cell::new(Some(0.0_f64)));
    let progress_for_draw = progress.clone();

    let area = gtk::DrawingArea::new();
    area.set_content_width(22);
    area.set_content_height(22);
    area.set_size_request(22, 22);

    area.set_draw_func(move |area, cr, width, height| {
        let size = f64::from(width.min(height)).max(1.0);
        let center = size / 2.0;
        let line_width = 2.4;
        let radius = (size / 2.0 - line_width - 1.0).max(1.0);
        let fraction = progress_for_draw.get().unwrap_or(0.0_f64).clamp(0.0, 1.0);
        let (r, g, b) = widget_css_color(&area.clone().upcast::<gtk::Widget>());

        cr.set_line_width(line_width);
        cr.set_line_cap(gtk::cairo::LineCap::Round);

        cr.set_source_rgba(r, g, b, 0.28);
        cr.arc(center, center, radius, 0.0, TAU);
        let _ = cr.stroke();

        if fraction > 0.0 {
            cr.set_source_rgba(r, g, b, 1.0);
            cr.arc(
                center,
                center,
                radius,
                -FRAC_PI_2,
                -FRAC_PI_2 + TAU * fraction,
            );
            let _ = cr.stroke();
        }
    });

    let button = Button::new();
    button.set_child(Some(&area));
    button.set_tooltip_text(Some(&gettext("Update progress")));
    button.set_visible(false);
    button.add_css_class("circular");
    button.add_css_class("flat");

    (button, area, progress)
}

#[cfg(target_os = "windows")]
fn widget_css_color(widget: &gtk::Widget) -> (f64, f64, f64) {
    let ctx = widget.style_context();
    if let Some(c) = ctx.lookup_color("color") {
        return (c.red() as f64, c.green() as f64, c.blue() as f64);
    }
    if let Some(c) = ctx.lookup_color("theme_fg_color") {
        return (c.red() as f64, c.green() as f64, c.blue() as f64);
    }
    if let Some(c) = ctx.lookup_color("window_fg_color") {
        return (c.red() as f64, c.green() as f64, c.blue() as f64);
    }

    (1.0, 1.0, 1.0)
}
