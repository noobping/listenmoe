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

<a href="https://flathub.org/apps/details/io.github.noobping.listenmoe">
  <img alt="Get it on Flathub" src="https://flathub.org/api/badge?locale=en"/>
</a>

## Translations

The `po` folder contains translation files in `.po` (Portable Object) format. If you spot a typo, unclear wording, or have a better translation, contributions are welcome.

## Development

### Build

Build the AppImage:

```sh
./.appimage-po.sh
appimage-builder --recipe .appimage-builder.yml
```

### Run (debug)

```sh
cargo run
```

### Update

Use `cargo-edit` to update the dependencies.

```sh
cargo install cargo-edit
```

```sh
cargo upgrade --incompatible
```
