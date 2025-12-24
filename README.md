# The world's cutest radio

![License](https://img.shields.io/badge/license-MIT-blue.svg)
[![Windows Build](https://github.com/noobping/listenmoe/actions/workflows/win.yml/badge.svg)](https://github.com/noobping/listenmoe/actions/workflows/win.yml)
[![Linux Build](https://github.com/noobping/listenmoe/actions/workflows/linux.yml/badge.svg)](https://github.com/noobping/listenmoe/actions/workflows/linux.yml)
[![Flathub version](https://img.shields.io/flathub/v/io.github.noobping.listenmoe)](https://flathub.org/apps/details/io.github.noobping.listenmoe)

This is a Unofficial App for LISTEN.moe. Stream and metadata provided by [LISTEN.moe](https://listen.moe).
Listen to J-POP and K-POP, or pause and resume the live stream.

![screenshot](data/io.github.noobping.listenmoe.screenshot_green.png)

<a href="https://flathub.org/apps/details/io.github.noobping.listenmoe">
  <img alt="Get it on Flathub" src="https://flathub.org/api/badge?locale=en"/>
</a>

## Translations

The `po` folder contains translation files in `.po` (Portable Object) format. If you spot a typo, unclear wording, or have a better translation, contributions are welcome.

## Build

Build the flatpak App:

```sh
flatpak-builder --user --install --force-clean flatpak-build io.github.noobping.listenmoe.yml
```

Or build a AppImage:

```sh
./.appimage-po.sh
appimage-builder --recipe .appimage-builder.yml
```

## Run (debug)

```sh
cargo run
```
