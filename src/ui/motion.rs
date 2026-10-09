use adw::{glib, gtk, prelude::*};

pub(super) const REDUCED_MOTION_SETTING: &str = "gtk-interface-reduced-motion";

pub(super) fn animations_enabled(settings: &gtk::Settings) -> bool {
    settings.is_gtk_enable_animations() && !prefers_reduced_motion(settings)
}

fn prefers_reduced_motion(settings: &gtk::Settings) -> bool {
    // GTK added this property in 4.22. Inspect it at runtime so older GTK
    // installations can still use the existing animation preference.
    if settings.find_property(REDUCED_MOTION_SETTING).is_none() {
        return false;
    }
    let value = settings.property_value(REDUCED_MOTION_SETTING);
    glib::EnumValue::from_value(&value).is_some_and(|(_, motion)| motion.nick() == "reduce")
}

pub(super) fn connect_changed(
    settings: &gtk::Settings,
    changed: impl Fn(&gtk::Settings) + 'static,
) -> glib::SignalHandlerId {
    settings.connect_notify_local(None, move |settings, property| {
        if matches!(
            property.name(),
            "gtk-enable-animations" | REDUCED_MOTION_SETTING
        ) {
            changed(settings);
        }
    })
}
