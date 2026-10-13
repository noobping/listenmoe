
# Translations

This folder contains translation files in `.po` (Portable Object) format. If you spot a typo, unclear wording, or have a better translation, contributions are welcome.

Filenames use gettext locale names: `en_GB` for British English, `pt_BR` for Brazilian Portuguese, `nb` for Norwegian Bokmål, and `zh_CN`/`zh_TW` for Simplified/Traditional Chinese. These correspond to Flathub's `en-GB`, `pt-BR`, `nb-NO`, `zh-Hans`, and `zh-Hant` options. `fil` is Filipino.

Meson builds and installs all catalogs listed in `LINGUAS` and merges translations into the desktop and AppStream files. The release Flatpak and AppImage builds already discover `po/*.po`. Windows embeds the same catalogs in `src/locale/embedded.rs`; update that list and `LINGUAS` when adding a language. Its tests check coverage, placeholders, regional variants, and fallback behavior.

To validate a catalog, run `msgfmt --check po/<locale>.po -o /tmp/listenmoe.mo`. To use translations with `cargo run`, compile them first with `sh .appimage-po.sh data/locale`.

## What to edit

Each `.po` file contains entries like:

```text
msgid "Original text"
msgstr "Translated text"
```

Only edit the msgstr line. Do not change msgid.

## Edit on GitHub

1. Open a `.po` file in this folder on GitHub
2. Click the Edit (pencil) button
3. GitHub will ask you to fork the repository, accept it
4. Improve the `msgstr` text
5. Propose the changes and create a Pull Request

## Alternative

If you do not want to edit files on GitHub, you can open an [issue](https://github.com/noobping/listenmoe/issues) instead. In the issue, include the original text (`msgid`), your suggested translation (`msgstr`), and the language file name (for example `ja.po`).

## Guidelines

Keep the meaning of the original text the same. Do not add or remove information unless necessary for the language. Placeholders such as `%s`, `{name}`, or `%(count)d` must remain exactly the same. They may be moved if required by grammar, but must not be changed or removed. Do not edit `msgid`, comments, or file headers. Only modify the `msgstr` values. **Exception:** These rules do not apply to the translation notes (e.g. AI translation or review annotations).
