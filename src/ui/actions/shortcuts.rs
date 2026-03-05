use adw::glib;
use adw::gtk;
use adw::prelude::*;
use gettextrs::gettext;

pub fn install_shortcuts_overlay(window: &gtk::ApplicationWindow) {
    let esc = |s: String| glib::markup_escape_text(&s).to_string();
    let ui = format!(
        r#"
<?xml version="1.0" encoding="UTF-8"?>
<interface>
  <object class="GtkShortcutsWindow" id="shortcuts">
    <property name="title">{title}</property>
    <child>
      <object class="GtkShortcutsSection">
        <property name="section-name">general</property>
        <property name="title">{general}</property>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">{playback}</property>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">{play_or_pause}</property>
                <property name="accelerator">&lt;Primary&gt;p</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">{stop_playback}</property>
                <property name="accelerator">XF86AudioStop</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">{copy_current_track}</property>
                <property name="accelerator">&lt;Primary&gt;c</property>
              </object>
            </child>
          </object>
        </child>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">{stations}</property>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">{play_jpop}</property>
                <property name="accelerator">&lt;Primary&gt;j</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">{play_kpop}</property>
                <property name="accelerator">&lt;Primary&gt;k</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">{prev_station}</property>
                <property name="accelerator">&lt;Primary&gt;z</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">{next_station}</property>
                <property name="accelerator">&lt;Primary&gt;y</property>
              </object>
            </child>
          </object>
        </child>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">{window_group}</property>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">{open_preferences}</property>
                <property name="accelerator">&lt;Primary&gt;comma</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">{show_shortcuts}</property>
                <property name="accelerator">&lt;Primary&gt;question</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">{quit}</property>
                <property name="accelerator">&lt;Primary&gt;q</property>
              </object>
            </child>
          </object>
        </child>
      </object>
    </child>
  </object>
</interface>
"#,
        title = esc(gettext("Keyboard Shortcuts")),
        general = esc(gettext("General")),
        playback = esc(gettext("Playback")),
        play_or_pause = esc(gettext("Play or pause")),
        stop_playback = esc(gettext("Stop playback")),
        copy_current_track = esc(gettext("Copy current track")),
        stations = esc(gettext("Stations")),
        play_jpop = esc(gettext("Play J-POP")),
        play_kpop = esc(gettext("Play K-POP")),
        prev_station = esc(gettext("Previous station")),
        next_station = esc(gettext("Next station")),
        window_group = esc(gettext("Window")),
        open_preferences = esc(gettext("Open preferences")),
        show_shortcuts = esc(gettext("Show shortcuts")),
        quit = esc(gettext("Quit")),
    );

    let builder = gtk::Builder::from_string(ui.as_str());
    let shortcuts: gtk::ShortcutsWindow = builder
        .object("shortcuts")
        .expect("Failed to build shortcuts window");
    shortcuts.set_transient_for(Some(window));
    shortcuts.set_modal(true);
    shortcuts.set_hide_on_close(true);

    window.set_help_overlay(Some(&shortcuts));
}
