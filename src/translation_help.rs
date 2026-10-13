use crate::{locale::gettext, preferences};
use adw::gtk::gio::{self, AppInfo, Notification, SimpleAction};
use adw::prelude::*;
use adw::Application;

pub(crate) const TRANSLATION_URL: &str = "https://hosted.weblate.org/projects/listenmoe/";
const NOTIFICATION_ID: &str = "translation-help";
const OPEN_TRANSLATION_ACTION: &str = "open-translation-project";
const OPEN_TRANSLATION_DETAILED_ACTION: &str = "app.open-translation-project";

pub(crate) fn register_app_actions(app: &Application) {
    let action = SimpleAction::new(OPEN_TRANSLATION_ACTION, None);
    action.connect_activate(|_, _| {
        let context = adw::gtk::gdk::Display::default().map(|display| display.app_launch_context());
        AppInfo::launch_default_for_uri_async(
            TRANSLATION_URL,
            context.as_ref(),
            None::<&gio::Cancellable>,
            |result| {
                if let Err(error) = result {
                    eprintln!("Failed to open the translation project: {error}");
                }
            },
        );
    });
    app.add_action(&action);
}

pub(crate) fn show_notification_once(app: &Application) {
    if !desktop_notification_delivery_available(app) {
        return;
    }

    match preferences::translation_help_notification_sent() {
        Ok(true) => return,
        Ok(false) => (),
        Err(error) => {
            eprintln!("Failed to read the translation-help notification state: {error}");
            return;
        }
    }
    if let Err(error) = preferences::mark_translation_help_notification_sent() {
        eprintln!("Failed to save the translation-help notification state: {error}");
        return;
    }

    let notification = Notification::new(&gettext("Help translate Listen Moe"));
    notification.set_body(Some(&gettext(
        "Help improve Listen Moe translations on Weblate.",
    )));
    notification.add_button(&gettext("Open Weblate"), OPEN_TRANSLATION_DETAILED_ACTION);
    notification.set_default_action(OPEN_TRANSLATION_DETAILED_ACTION);
    app.send_notification(Some(NOTIFICATION_ID), &notification);
}

fn desktop_notification_delivery_available(app: &Application) -> bool {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        // GLib needs an installed desktop file. Keep the invitation pending for
        // uninstalled development builds, including an overridden application ID.
        let Some(app_id) = app.application_id() else {
            return false;
        };
        let desktop_id = format!("{app_id}.desktop");
        AppInfo::all()
            .iter()
            .any(|info| info.id().as_deref() == Some(desktop_id.as_str()))
    }

    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        let _ = app;
        true
    }
}
