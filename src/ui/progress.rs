use adw::gtk::{self, prelude::*};
use std::{cell::RefCell, rc::Rc};

const PROGRESS_HEIGHT: i32 = 3;
const LINE_WIDTH: f64 = 2.0;

#[derive(Clone)]
pub(crate) struct TitlebarProgress {
    area: gtk::DrawingArea,
    state: Rc<RefCell<ProgressState>>,
}

#[derive(Default)]
struct ProgressState {
    track_fraction: Option<f64>,
    update_fraction: Option<f64>,
}

impl ProgressState {
    fn visible_fraction(&self) -> Option<f64> {
        self.update_fraction.or(self.track_fraction)
    }
}

impl TitlebarProgress {
    #[cfg(all(test, target_os = "windows"))]
    pub(crate) fn for_test() -> Self {
        make_titlebar_progress().1
    }

    pub(super) fn set_track_fraction(&self, fraction: Option<f64>) {
        self.set_fraction(fraction, false);
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn set_update_fraction(&self, fraction: Option<f64>) {
        self.set_fraction(fraction, true);
    }

    fn set_fraction(&self, fraction: Option<f64>, is_update: bool) {
        let fraction = fraction.map(|value| value.clamp(0.0, 1.0));
        let mut state = self.state.borrow_mut();
        let previous = state.visible_fraction();
        if is_update {
            state.update_fraction = fraction;
        } else {
            state.track_fraction = fraction;
        }
        let current = state.visible_fraction();
        drop(state);

        if previous == current {
            return;
        }

        self.area.queue_draw();
    }
}

pub(super) fn make_titlebar_progress() -> (gtk::DrawingArea, TitlebarProgress) {
    let state = Rc::new(RefCell::new(ProgressState::default()));
    let state_for_draw = state.clone();

    let area = gtk::DrawingArea::new();
    area.set_can_target(false);
    area.set_hexpand(true);
    area.set_halign(gtk::Align::Fill);
    area.set_valign(gtk::Align::End);
    area.set_content_height(PROGRESS_HEIGHT);
    area.set_height_request(PROGRESS_HEIGHT);
    area.add_css_class("titlebar-progress");

    area.set_draw_func(move |area, cr, width, height| {
        let Some(fraction) = state_for_draw.borrow().visible_fraction() else {
            return;
        };

        let width = f64::from(width);
        let height = f64::from(height);
        if width <= LINE_WIDTH || height <= 0.0 {
            return;
        }

        let (r, g, b) = widget_css_color(&area.clone().upcast::<gtk::Widget>());
        let inset = LINE_WIDTH / 2.0;
        let start_x = inset;
        let end_x = (width - inset).max(start_x);
        let y = inset;

        cr.set_line_width(LINE_WIDTH);
        cr.set_line_cap(gtk::cairo::LineCap::Round);

        cr.set_source_rgba(r, g, b, 0.30);
        cr.move_to(start_x, y);
        cr.line_to(end_x, y);
        let _ = cr.stroke();

        if fraction > 0.0 {
            cr.set_source_rgba(r, g, b, 0.92);
            cr.move_to(start_x, y);
            cr.line_to(start_x + (end_x - start_x) * fraction, y);
            let _ = cr.stroke();
        }
    });

    let progress = TitlebarProgress {
        area: area.clone(),
        state,
    };
    (area, progress)
}

fn widget_css_color(widget: &gtk::Widget) -> (f64, f64, f64) {
    let color = widget.style_context().color();
    (
        color.red() as f64,
        color.green() as f64,
        color.blue() as f64,
    )
}

#[cfg(test)]
mod tests {
    use super::ProgressState;

    #[test]
    fn update_progress_temporarily_overrides_the_latest_track_progress() {
        let mut state = ProgressState {
            track_fraction: Some(0.25),
            update_fraction: None,
        };
        assert_eq!(state.visible_fraction(), Some(0.25));

        state.update_fraction = Some(0.6);
        assert_eq!(state.visible_fraction(), Some(0.6));

        state.track_fraction = Some(0.4);
        assert_eq!(state.visible_fraction(), Some(0.6));

        state.update_fraction = None;
        assert_eq!(state.visible_fraction(), Some(0.4));
    }
}
