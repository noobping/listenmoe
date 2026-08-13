use crate::locale::gettext;
use adw::gtk::{
    accessible::Property,
    prelude::{AccessibleExtManual, ScaleButtonExt, WidgetExt},
    ScaleButton,
};
use std::{cell::Cell, rc::Rc};

const MIN_PERCENT: f64 = 0.0;
const MAX_PERCENT: f64 = 100.0;
const STEP_PERCENT: f64 = 2.0;

pub(super) struct VolumeUi {
    button: ScaleButton,
    suppress_change: Rc<Cell<bool>>,
    on_change: Rc<dyn Fn(u8)>,
}

pub(super) fn build_button() -> ScaleButton {
    // GtkScaleButton treats the first two icons as its minimum and maximum,
    // then distributes any remaining icons between them.
    let button = ScaleButton::new(
        MIN_PERCENT,
        MAX_PERCENT,
        STEP_PERCENT,
        &[
            "audio-volume-muted-symbolic",
            "audio-volume-high-symbolic",
            "audio-volume-low-symbolic",
            "audio-volume-medium-symbolic",
        ],
    );
    let label = gettext("Volume");
    button.set_tooltip_text(Some(&label));
    button.update_property(&[Property::Label(&label)]);
    button
}

impl VolumeUi {
    pub(super) fn new(button: ScaleButton, initial_percent: u8, on_change: Rc<dyn Fn(u8)>) -> Self {
        let initial_percent = initial_percent.min(100);
        let suppress_change = Rc::new(Cell::new(false));

        button.set_visible(true);
        button.set_value(f64::from(initial_percent));

        {
            let suppress_change = suppress_change.clone();
            let on_change = on_change.clone();
            button.connect_value_changed(move |_, value| {
                if suppress_change.get() {
                    return;
                }

                let percent = display_percent(value);
                on_change(percent);
            });
        }

        Self {
            button,
            suppress_change,
            on_change,
        }
    }

    /// Apply a volume request from outside GTK, such as MPRIS.
    ///
    /// This deliberately uses the same callback as a direct button change so
    /// every user-facing control follows the same controller/fallback path.
    pub(super) fn request_percent(&self, percent: u8) {
        let percent = percent.min(100);
        self.set_percent_silent(percent);
        (self.on_change)(percent);
    }

    /// Reflect an observed backend value without turning it into a new request.
    pub(super) fn set_percent_silent(&self, percent: u8) {
        let percent = percent.min(100);
        self.suppress_change.set(true);
        self.button.set_value(f64::from(percent));
        self.suppress_change.set(false);
    }
}

fn display_percent(value: f64) -> u8 {
    if value.is_nan() {
        return 0;
    }

    value.clamp(MIN_PERCENT, MAX_PERCENT).round() as u8
}

#[cfg(test)]
mod tests {
    use super::display_percent;

    #[test]
    fn display_percent_is_clamped_and_rounded() {
        assert_eq!(display_percent(-1.0), 0);
        assert_eq!(display_percent(49.4), 49);
        assert_eq!(display_percent(49.5), 50);
        assert_eq!(display_percent(101.0), 100);
        assert_eq!(display_percent(f64::NAN), 0);
    }
}
