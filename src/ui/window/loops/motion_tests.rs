use super::*;
use crate::ui::{motion::REDUCED_MOTION_SETTING, viz::make_bars_visualizer};
use adw::gtk::prelude::*;

fn settle_frames() {
    let context = glib::MainContext::default();
    let until = Instant::now() + Duration::from_millis(250);
    while Instant::now() < until {
        while context.pending() {
            context.iteration(false);
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn pixels(window: &gtk::Window) -> Vec<u8> {
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(window)).snapshot(
        &snapshot,
        f64::from(window.width()),
        f64::from(window.height()),
    );
    let texture = window
        .renderer()
        .unwrap()
        .render_texture(&snapshot.to_node().unwrap(), None);
    let mut pixels = vec![0; (texture.width() * texture.height() * 4) as usize];
    texture.download(&mut pixels, texture.width() as usize * 4);
    pixels
}

// Run this test by name in its own process to keep GTK on one test thread.
#[test]
#[ignore = "requires a GTK display; run separately from other GTK tests"]
fn gtk_visualizer_respects_motion_and_releases_widget() {
    gtk::init().unwrap();
    let settings = gtk::Settings::default().unwrap();
    let original_animations = settings.is_gtk_enable_animations();
    let original_motion = settings
        .find_property(REDUCED_MOTION_SETTING)
        .map(|_| settings.property_value(REDUCED_MOTION_SETTING));
    let set_reduced_motion = |reduce| {
        if let Some(original) = &original_motion {
            let class = glib::EnumClass::with_type(original.type_()).unwrap();
            settings.set_property_from_value(
                REDUCED_MOTION_SETTING,
                &class
                    .to_value_by_nick(if reduce { "reduce" } else { "no-preference" })
                    .unwrap(),
            );
        }
    };
    set_reduced_motion(true);
    settings.set_gtk_enable_animations(original_motion.is_some());
    let (viz, handle) = make_bars_visualizer(4, 50);
    let weak_viz = viz.downgrade();
    let spectrum = Arc::new((0..4).map(|_| AtomicU32::new(1.0f32.to_bits())).collect());
    spawn_viz_loop(viz.clone(), handle, spectrum);
    let window = gtk::Window::builder()
        .default_width(300)
        .default_height(50)
        .decorated(false)
        .child(&viz)
        .build();
    window.present();
    settle_frames();
    let dimensions = (window.width(), window.height());
    let plain = pixels(&window);

    set_reduced_motion(false);
    settings.set_gtk_enable_animations(true);
    settle_frames();
    assert!(pixels(&window) != plain, "bars must animate when permitted");

    if original_motion.is_some() {
        set_reduced_motion(true);
        settle_frames();
        assert!(
            pixels(&window) == plain,
            "Reduce Motion must clear the bars"
        );
        window.set_visible(false);
        window.present();
        settle_frames();
        assert!(
            pixels(&window) == plain,
            "showing the window must respect Reduce Motion"
        );

        // Clearing Reduce Motion must not override the older animation setting.
        settings.set_gtk_enable_animations(false);
        set_reduced_motion(false);
        settle_frames();
        assert!(pixels(&window) == plain);
        settings.set_gtk_enable_animations(true);
        settle_frames();
        assert!(
            pixels(&window) != plain,
            "bars must resume after re-enabling motion"
        );
    }

    settings.set_gtk_enable_animations(false);
    settle_frames();
    assert!(
        pixels(&window) == plain,
        "disabled animations must clear the bars too"
    );
    assert_eq!((window.width(), window.height()), dimensions);

    // Destroy with an active timer to check that neither it nor the settings
    // handler keeps the drawing area alive.
    settings.set_gtk_enable_animations(true);
    settle_frames();
    window.destroy();
    drop(window);
    drop(viz);
    settle_frames();
    assert!(weak_viz.upgrade().is_none());

    settings.set_gtk_enable_animations(original_animations);
    if let Some(original) = original_motion {
        settings.set_property_from_value(REDUCED_MOTION_SETTING, &original);
    }
}
