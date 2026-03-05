# The world's cutest radio

![License](https://img.shields.io/badge/license-MIT-blue.svg)
[![Windows Build](https://github.com/noobping/listenmoe/actions/workflows/win.yml/badge.svg)](https://github.com/noobping/listenmoe/actions/workflows/win.yml)
[![Linux Build](https://github.com/noobping/listenmoe/actions/workflows/linux.yml/badge.svg)](https://github.com/noobping/listenmoe/actions/workflows/linux.yml)
[![Flathub version](https://img.shields.io/flathub/v/io.github.noobping.listenmoe)](https://flathub.org/apps/details/io.github.noobping.listenmoe)
[![Get it for Windows](https://img.shields.io/badge/Get%20it%20on-Windows-blue)](https://github.com/noobping/listenmoe/releases/latest/download/listenmoe.msi)

Listen to J-POP and K-POP, or pause and resume the live stream. Stream and metadata provided by [LISTEN.moe](https://listen.moe).

![demo](data/demo.gif)

The application uses a compact, titlebar-style layout that displays the current album and artist, along with basic playback controls.

When album or artist artwork is available, a dominant color is extracted and used to select the appropriate GNOME light or dark appearance. If no artwork is available, the default GNOME appearance is used.

The background includes subtle, animated sound bars that respond to the music. Their color adapts to the extracted palette while remaining unobtrusive. Text readability is preserved using a soft overlay behind the title and subtitle.

## Installation

You can install ListenMoe using one of the following options:

- **Windows / Linux (AppImage):**  
  Download the latest release from the [GitHub releases page](https://github.com/noobping/listenmoe/releases/latest).
- **Linux (Flatpak):**  
  Install it from [Flathub](https://flathub.org/apps/details/io.github.noobping.listenmoe).

<a href="https://flathub.org/apps/details/io.github.noobping.listenmoe">
  <img alt="Get it on Flathub" src="https://flathub.org/api/badge?locale=en"/>
</a>

## Options

The application can be started with optional flags. For example:

```sh
flatpak run io.github.noobping.listenmoe --autoplay --kpop --stop --no-discord --preferences
```

Available flags:

- `-a`, `--autoplay`: start playing automatically on launch
- `-j`, `--jpop`: use J-POP as default station
- `-k`, `--kpop`: use K-POP as default station
- `-p`, `--preferences`: save current startup flags as defaults
- `--no-discord`: disable Discord Rich Presence at runtime
- `-s`, `--stop`: use stop behavior instead of pause
- `-v`, `--verbose`: print extra startup diagnostics
- `-h`, `--help`: show help and exit
- `--version`: show version and exit

Keyboard shortcuts:

- `Ctrl+,`: open Preferences
- `Ctrl+?` or `F1`: open Keyboard Shortcuts

## Translations

The `po` folder contains translation files in `.po` (Portable Object) format. If you spot a typo, unclear wording, or have a better translation, contributions are welcome.

## Development

Dependencies:

```sh
sudo dnf install -y @development-tools cargo clang gcc gcc-c++ gettext libadwaita-devel alsa-lib-devel cairo-devel gdk-pixbuf2-devel glib2-devel libgpg-error-devel gtk4-devel pango-devel openssl-devel make mold nettle-devel pkgconf-pkg-config pkgconf
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
```

Build the AppImage:

```sh
./.appimage-po.sh
appimage-builder --recipe .appimage-builder.yml
```

Run (debug):

```sh
glib-compile-schemas data
GSETTINGS_SCHEMA_DIR=data cargo run -p
```

Use `cargo-edit` to update the dependencies.

```sh
cargo install cargo-edit
```

```sh
cargo upgrade --incompatible
```
