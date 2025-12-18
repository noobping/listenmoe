# The world's cutest radio

![License](https://img.shields.io/badge/license-MIT-blue.svg)
[![Windows Build](https://github.com/noobping/listenmoe/actions/workflows/win.yml/badge.svg)](https://github.com/noobping/listenmoe/actions/workflows/win.yml)
[![Linux Build](https://github.com/noobping/listenmoe/actions/workflows/linux.yml/badge.svg)](https://github.com/noobping/listenmoe/actions/workflows/linux.yml)

The world's cutest radio. Dive into pure kawaii energy with nonstop Japanese and Korean hits, streamed straight from [LISTEN.moe](https://listen.moe/).

![screenshot](data/io.github.noobping.listenmoe.screenshot.png)

## Flatpak App

Install the app from my Flatpak Repo:

```sh
flatpak remote-add --if-not-exists flatpaks https://noobping.github.io/flatpaks/flatpaks.flatpakrepo ;\
flatpak install flatpaks io.github.noobping.listenmoe
```

## AppImage

This application is an internet radio client. It streams music from the internet, so it needs network access.
If you run this app inside **firejail**, make sure the firejail profile allows network access. Otherwise it won’t be able to stream any audio.
To install the provided firejail profile:

```sh
mkdir -p ~/.config/firejail
cp /path/to/extracted/usr/share/firejail/listenmoe.profile ~/.config/firejail/
firejail listenmoe
```

## Translations

The `po/` folder contains translation files in `.po` (Portable Object) format. If you find a typo, unclear wording, or can improve a translation, you are welcome to contribute.
