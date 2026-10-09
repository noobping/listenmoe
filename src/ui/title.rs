use adw::{
    glib,
    gtk::{self, graphene, pango, prelude::*, subclass::prelude::*},
};
use std::cell::{Cell, RefCell};

const SCROLL_SPEED: f64 = 20.0;
const SEPARATOR: &str = " ・ ";

fn scroll_offset(elapsed_us: i64, period: f64) -> f64 {
    if period <= 0.0 {
        return 0.0;
    }
    (elapsed_us.max(0) as f64 / 1_000_000.0 * SCROLL_SPEED) % period
}

mod row {
    use super::*;

    #[derive(Default)]
    pub struct MarqueeRow {
        pub label: gtk::Label,
        pub started_at: Cell<Option<i64>>,
        pub offset: Cell<f64>,
        // Keep the source layout as well as its serial: GTK may replace it on
        // a font/style change with a new layout that has the same serial.
        pub layout: RefCell<Option<(pango::Layout, u32, pango::Layout)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MarqueeRow {
        const NAME: &'static str = "ListenMoeMarqueeRow";
        type Type = super::MarqueeRow;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for MarqueeRow {
        fn constructed(&self) {
            self.parent_constructed();
            self.label.set_ellipsize(pango::EllipsizeMode::End);
            self.label.set_single_line_mode(true);
            self.label.set_parent(&*self.obj());
        }

        fn dispose(&self) {
            self.label.unparent();
        }
    }

    impl WidgetImpl for MarqueeRow {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.label.measure(orientation, for_size)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.label.allocate(width, height, baseline, None);
            if let Some(title) = self.obj().parent().and_downcast::<ScrollingWindowTitle>() {
                title.update_animation();
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            if !obj.should_scroll() {
                obj.snapshot_child(&self.label, snapshot);
                return;
            }

            let layout = obj.loop_layout();
            let period = layout.pixel_size().0 as f32;
            let Some(origin) = self.label.compute_point(&*obj, &graphene::Point::zero()) else {
                return;
            };
            let (_, y) = self.label.layout_offsets();
            let baseline_adjustment =
                (self.label.layout().baseline() - layout.baseline()) as f32 / pango::SCALE as f32;
            let color = self.label.style_context().color();

            // Draw only inside the label's content box, preserving its CSS
            // padding. The actual label still supplies sizing and accessibility.
            snapshot.push_clip(&graphene::Rect::new(
                origin.x(),
                origin.y(),
                self.label.width() as f32,
                self.label.height() as f32,
            ));
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                origin.x() - (self.offset.get() % f64::from(period)) as f32,
                origin.y() + y as f32 + baseline_adjustment,
            ));
            snapshot.append_layout(&layout, &color);
            snapshot.translate(&graphene::Point::new(period, 0.0));
            snapshot.append_layout(&layout, &color);
            snapshot.restore();
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    pub struct MarqueeRow(ObjectSubclass<row::MarqueeRow>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for MarqueeRow {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl MarqueeRow {
    fn set_text(&self, text: &str) {
        if self.imp().label.text() == text {
            return;
        }
        self.imp().label.set_text(text);
        self.set_visible(!text.is_empty());
        self.imp().layout.take();
        self.reset();
    }

    fn should_scroll(&self) -> bool {
        self.is_visible()
            && self.settings().is_gtk_enable_animations()
            && self.imp().label.width() > 0
            && self.imp().label.layout().is_ellipsized()
    }

    fn loop_layout(&self) -> pango::Layout {
        let source = self.imp().label.layout();
        let serial = source.serial();
        let mut cached = self.imp().layout.borrow_mut();
        if let Some((previous, previous_serial, layout)) = cached.as_ref() {
            if previous == &source && *previous_serial == serial {
                return layout.clone();
            }
        }
        let layout = source.copy();
        layout.set_width(-1);
        layout.set_ellipsize(pango::EllipsizeMode::None);
        layout.set_alignment(pango::Alignment::Left);
        layout.set_text(&format!("{}{SEPARATOR}", self.imp().label.text()));
        *cached = Some((source, serial, layout.clone()));
        layout
    }

    fn tick(&self, frame_time: i64) {
        if !self.should_scroll() {
            self.reset();
            return;
        }
        let start = self.imp().started_at.get().unwrap_or(frame_time);
        self.imp().started_at.set(Some(start));
        let period = f64::from(self.loop_layout().pixel_size().0);
        self.imp()
            .offset
            .set(scroll_offset(frame_time - start, period));
        self.queue_draw();
    }

    fn reset(&self) {
        self.imp().started_at.set(None);
        if self.imp().offset.replace(0.0) != 0.0 {
            self.queue_draw();
        }
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ScrollingWindowTitle {
        pub title: MarqueeRow,
        pub subtitle: MarqueeRow,
        pub tick: RefCell<Option<gtk::TickCallbackId>>,
        pub settings_handler: RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ScrollingWindowTitle {
        const NAME: &'static str = "ListenMoeScrollingWindowTitle";
        type Type = super::ScrollingWindowTitle;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            // Reuse the native WindowTitle styling, including its margins.
            klass.set_css_name("windowtitle");
        }
    }

    impl ObjectImpl for ScrollingWindowTitle {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_orientation(gtk::Orientation::Vertical);
            obj.set_valign(gtk::Align::Center);
            // Style the clipping widgets themselves so GTK applies CSS opacity
            // (notably the dim subtitle) to both static and scrolling rendering.
            self.title.add_css_class("title");
            self.title.imp().label.set_width_chars(5);
            self.subtitle.add_css_class("subtitle");
            self.title.set_visible(false);
            self.subtitle.set_visible(false);
            obj.append(&self.title);
            obj.append(&self.subtitle);

            let weak = obj.downgrade();
            *self.settings_handler.borrow_mut() = Some(
                obj.settings()
                    .connect_gtk_enable_animations_notify(move |_| {
                        if let Some(obj) = weak.upgrade() {
                            obj.update_animation();
                            obj.imp().title.queue_draw();
                            obj.imp().subtitle.queue_draw();
                        }
                    }),
            );
        }

        fn dispose(&self) {
            self.obj().stop_animation();
            if let Some(handler) = self.settings_handler.take() {
                self.obj().settings().disconnect(handler);
            }
        }
    }

    impl WidgetImpl for ScrollingWindowTitle {
        fn map(&self) {
            self.parent_map();
            self.obj().update_animation();
        }

        fn unmap(&self) {
            self.obj().stop_animation();
            self.parent_unmap();
        }
    }

    impl BoxImpl for ScrollingWindowTitle {}
}

glib::wrapper! {
    pub struct ScrollingWindowTitle(ObjectSubclass<imp::ScrollingWindowTitle>)
        @extends gtk::Widget, gtk::Box,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl ScrollingWindowTitle {
    pub(crate) fn new(title: &str, subtitle: &str) -> Self {
        let obj: Self = glib::Object::new();
        obj.set_title(title);
        obj.set_subtitle(subtitle);
        obj
    }

    pub(crate) fn set_title(&self, title: &str) {
        self.imp().title.set_text(title);
        self.update_tooltip();
        self.update_animation();
    }

    pub(crate) fn set_subtitle(&self, subtitle: &str) {
        self.imp().subtitle.set_text(subtitle);
        self.update_tooltip();
        self.update_animation();
    }

    fn update_tooltip(&self) {
        let title = self.imp().title.imp().label.text();
        let subtitle = self.imp().subtitle.imp().label.text();
        let text = [title.as_str(), subtitle.as_str()]
            .into_iter()
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        self.set_tooltip_text(if text.is_empty() { None } else { Some(&text) });
    }

    fn update_animation(&self) {
        let imp = self.imp();
        if !self.is_mapped() || !(imp.title.should_scroll() || imp.subtitle.should_scroll()) {
            self.stop_animation();
            return;
        }
        if imp.tick.borrow().is_some() {
            return;
        }
        // The callback's widget argument avoids a reference cycle.
        *imp.tick.borrow_mut() = Some(self.add_tick_callback(|obj, clock| {
            obj.imp().title.tick(clock.frame_time());
            obj.imp().subtitle.tick(clock.frame_time());
            glib::ControlFlow::Continue
        }));
    }

    fn stop_animation(&self) {
        if let Some(tick) = self.imp().tick.take() {
            tick.remove();
        }
        self.imp().title.reset();
        self.imp().subtitle.reset();
    }
}

#[cfg(test)]
mod tests;
