use super::super::motion::REDUCED_MOTION_SETTING;
use super::*;

#[test]
fn scroll_speed_is_independent_of_frame_rate_and_text_length() {
    assert_eq!(scroll_offset(500_000, 100.0), 10.0);
    assert_eq!(scroll_offset(500_000, 250.0), 10.0);
    assert_eq!(scroll_offset(2_500_000, 100.0), 50.0);
}

#[test]
fn each_line_wraps_at_its_own_period_without_a_pause() {
    assert_eq!(scroll_offset(5_000_000, 100.0), 0.0);
    assert_eq!(scroll_offset(5_000_000, 150.0), 100.0);
    assert!((scroll_offset(5_010_000, 100.0) - 0.2).abs() < 1e-9);
    assert_eq!(scroll_offset(15_000_000, 100.0), 0.0);
}

#[test]
fn empty_or_reset_animation_has_no_offset() {
    assert_eq!(scroll_offset(1_000_000, 0.0), 0.0);
    assert_eq!(scroll_offset(0, 100.0), 0.0);
    assert_eq!(scroll_offset(-1_000_000, 100.0), 0.0);
}

fn settle_layout() {
    let context = glib::MainContext::default();
    let until = std::time::Instant::now() + std::time::Duration::from_millis(200);
    while std::time::Instant::now() < until {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn header_window(
    title: &impl IsA<gtk::Widget>,
) -> (gtk::Window, gtk::CenterBox, gtk::Box, gtk::Button) {
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    for icon in [
        "view-more-symbolic",
        "media-playback-start-symbolic",
        "audio-volume-high-symbolic",
    ] {
        buttons.append(&gtk::Button::from_icon_name(icon));
    }
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    let center = gtk::CenterBox::new();
    center.set_hexpand(true);
    center.set_center_widget(Some(title));
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.set_hexpand(true);
    row.append(&buttons);
    row.append(&center);
    row.append(&close);
    let header = gtk::HeaderBar::new();
    header.set_show_title_buttons(false);
    header.set_title_widget(Some(&row));
    header.set_height_request(50);
    let window = gtk::Window::builder()
        .default_width(300)
        .default_height(50)
        .resizable(false)
        .build();
    window.set_titlebar(Some(&header));
    window.set_child(Some(&gtk::Box::new(gtk::Orientation::Vertical, 0)));
    (window, center, buttons, close)
}

fn window_texture(window: &gtk::Window) -> gtk::gdk::Texture {
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(window)).snapshot(
        &snapshot,
        f64::from(window.width()),
        f64::from(window.height()),
    );
    let node = snapshot.to_node().expect("window is drawable");
    window.renderer().unwrap().render_texture(&node, None)
}

fn save_window(window: &gtk::Window, name: &str) {
    let Ok(directory) = std::env::var("LISTENMOE_TITLE_SCREENSHOTS") else {
        return;
    };
    std::fs::create_dir_all(&directory).unwrap();
    window_texture(window)
        .save_to_png(std::path::Path::new(&directory).join(format!("{name}.png")))
        .unwrap();
}

fn row_pixels(row: &MarqueeRow, window: &gtk::Window) -> Vec<u8> {
    let snapshot = gtk::Snapshot::new();
    row.imp().snapshot(&snapshot);
    let node = snapshot.to_node().unwrap();
    let bounds = graphene::Rect::new(0.0, 0.0, row.width() as f32, row.height() as f32);
    let texture = window
        .renderer()
        .unwrap()
        .render_texture(&node, Some(&bounds));
    texture_pixels(&texture)
}

fn texture_pixels(texture: &gtk::gdk::Texture) -> Vec<u8> {
    let mut pixels = vec![0; (texture.width() * texture.height() * 4) as usize];
    texture.download(&mut pixels, texture.width() as usize * 4);
    pixels
}

// Run this test by name with a GTK display and --ignored --test-threads=1.
// Use a separate process for each GTK test because GTK has thread affinity.
#[test]
#[ignore = "requires a GTK display"]
fn gtk_title_preserves_layout_and_scroll_lifecycle() {
    adw::init().unwrap();
    let settings = gtk::Settings::default().unwrap();
    let original_animations = settings.is_gtk_enable_animations();
    let original_reduced_motion = settings
        .find_property(REDUCED_MOTION_SETTING)
        .map(|_| settings.property_value(REDUCED_MOTION_SETTING));
    let set_reduced_motion = |reduce: bool| {
        if let Some(original) = &original_reduced_motion {
            let class = glib::EnumClass::with_type(original.type_()).unwrap();
            let value = class
                .to_value_by_nick(if reduce { "reduce" } else { "no-preference" })
                .unwrap();
            settings.set_property_from_value(REDUCED_MOTION_SETTING, &value);
        }
    };
    set_reduced_motion(false);
    settings.set_gtk_enable_animations(false);
    let original = adw::WindowTitle::new("Listen Moe", "J-POP and K-POP radio");
    let (window, center, buttons, close) = header_window(&original);
    window.present();
    settle_layout();
    let geometry = || {
        (
            window.width(),
            window.height(),
            buttons.allocation(),
            close.allocation(),
        )
    };
    let baseline = geometry();
    let baseline_pixels = texture_pixels(&window_texture(&window));
    save_window(&window, "native");

    let title = ScrollingWindowTitle::new("Listen Moe", "J-POP and K-POP radio");
    center.set_center_widget(Some(&title));
    settle_layout();
    assert_eq!(geometry(), baseline, "window and controls must not move");
    save_window(&window, "scrolling");
    assert!(
        baseline_pixels == texture_pixels(&window_texture(&window)),
        "static rendering must preserve native styling and text placement"
    );
    settings.set_gtk_enable_animations(true);

    title.set_title("短い");
    title.set_subtitle("짧은 곡");
    settle_layout();
    assert!(!title.imp().title.should_scroll());
    assert!(!title.imp().subtitle.should_scroll());
    assert!(title.imp().tick.borrow().is_none());
    save_window(&window, "short");

    let long_title = "とても長いアーティスト名とゲストアーティスト";
    let long_subtitle = "아주 긴 노래 제목과 또 다른 이야기 <live> & encore";
    title.set_title(long_title);
    title.set_subtitle(long_subtitle);
    settle_layout();
    assert_eq!(geometry(), baseline);
    assert!(title.imp().title.should_scroll());
    assert!(title.imp().subtitle.should_scroll());
    assert!(title.imp().tick.borrow().is_some());
    assert_eq!(
        title.tooltip_text().unwrap(),
        format!("{long_title}\n{long_subtitle}")
    );
    for row in [&title.imp().title, &title.imp().subtitle] {
        row.reset();
        row.tick(1_000_000);
        row.tick(2_000_000);
        assert_eq!(row.imp().offset.get(), 20.0);
        assert_eq!(
            row.loop_layout().text(),
            format!("{}{SEPARATOR}", row.imp().label.text())
        );
        row.imp().offset.set(0.0);
        let start = row_pixels(row, &window);
        row.imp()
            .offset
            .set(f64::from(row.loop_layout().pixel_size().0) - 5.0);
        let seam = row_pixels(row, &window);
        let origin = row
            .imp()
            .label
            .compute_point(row, &graphene::Point::zero())
            .unwrap();
        let left = origin.x() as usize;
        let width = row.imp().label.width() as usize;
        let stride = row.width() as usize * 4;
        for y in 0..row.height() as usize {
            let begin = y * stride + left * 4;
            let end = begin + (width - 5) * 4;
            assert_eq!(
                &start[begin..end],
                &seam[begin + 20..end + 20],
                "the second copy must match the first across the loop seam"
            );
        }
        row.imp().offset.set(20.0);
    }
    // Only the original, complete text exists in the accessible label.
    assert_eq!(title.imp().title.imp().label.text(), long_title);
    title.set_title(long_title);
    assert_eq!(title.imp().title.imp().offset.get(), 20.0);
    title.set_title(&format!("{long_title}二"));
    assert_eq!(title.imp().title.imp().offset.get(), 0.0);
    assert_eq!(title.imp().subtitle.imp().offset.get(), 20.0);
    settle_layout();
    save_window(&window, "long");

    settings.set_gtk_enable_animations(false);
    settle_layout();
    assert!(title.imp().tick.borrow().is_none());
    assert!(!title.imp().title.should_scroll());
    assert!(title.imp().title.imp().label.layout().is_ellipsized());
    assert_eq!(geometry(), baseline);
    save_window(&window, "animations-disabled");
    settings.set_gtk_enable_animations(true);
    settle_layout();
    assert!(title.imp().tick.borrow().is_some());

    if original_reduced_motion.is_some() {
        let tooltip = title.tooltip_text();
        for (animations, reduce) in [(true, true), (false, true), (false, false), (true, false)] {
            settings.set_gtk_enable_animations(animations);
            set_reduced_motion(reduce);
            let should_scroll = animations && !reduce;
            // The notify handler must stop/start the clock immediately.
            assert_eq!(title.imp().tick.borrow().is_some(), should_scroll);
            settle_layout();
            for row in [&title.imp().title, &title.imp().subtitle] {
                assert_eq!(row.should_scroll(), should_scroll);
                if !should_scroll {
                    assert_eq!(row.imp().offset.get(), 0.0);
                    assert_eq!(row.imp().started_at.get(), None);
                    assert!(row.imp().label.layout().is_ellipsized());
                }
            }
            assert_eq!(title.tooltip_text(), tooltip);
            assert_eq!(geometry(), baseline);
        }

        set_reduced_motion(true);
        window.set_visible(false);
        title.set_title(long_title);
        window.present();
        settle_layout();
        assert!(title.imp().tick.borrow().is_none());
        assert_eq!(title.imp().title.imp().label.text(), long_title);
        assert_eq!(
            title.tooltip_text().unwrap(),
            format!("{long_title}\n{long_subtitle}")
        );
        assert_eq!(geometry(), baseline);
        save_window(&window, "reduced-motion");

        set_reduced_motion(false);
        assert!(title.imp().tick.borrow().is_some());
        assert_eq!(title.imp().title.imp().started_at.get(), None);
        assert_eq!(title.imp().subtitle.imp().offset.get(), 0.0);
        settle_layout();
    }

    window.set_visible(false);
    assert!(title.imp().tick.borrow().is_none());
    assert_eq!(title.imp().title.imp().started_at.get(), None);
    assert_eq!(title.imp().subtitle.imp().offset.get(), 0.0);
    window.present();
    settle_layout();
    assert!(title.imp().tick.borrow().is_some());

    title.set_title("Listen Moe");
    title.set_subtitle("Connecting...");
    settle_layout();
    assert_eq!(title.tooltip_text().unwrap(), "Listen Moe\nConnecting...");
    title.set_subtitle("");
    settle_layout();
    assert!(!title.imp().subtitle.is_visible());
    assert_eq!(title.tooltip_text().unwrap(), "Listen Moe");
    title.set_title(long_title);
    settle_layout();
    assert!(title.imp().tick.borrow().is_some());
    title.set_title("");
    assert!(title.imp().tick.borrow().is_none());
    assert!(title.tooltip_text().is_none());

    let row = MarqueeRow::default();
    row.set_text("Exactly fitting text");
    let natural_width = row.measure(gtk::Orientation::Horizontal, -1).1;
    let natural_height = row.measure(gtk::Orientation::Vertical, natural_width).1;
    row.allocate(natural_width, natural_height, -1, None);
    assert!(!row.should_scroll(), "an exact fit must stay still");
    row.allocate(natural_width - 1, natural_height, -1, None);
    assert!(row.should_scroll(), "one pixel of overflow must scroll");
    window.close();
    settings.set_gtk_enable_animations(original_animations);
    if let Some(original) = original_reduced_motion {
        settings.set_property_from_value(REDUCED_MOTION_SETTING, &original);
    }
}
