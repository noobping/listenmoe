
# Translations

This folder contains translation files in `.po` (Portable Object) format. If you spot a typo, unclear wording, or have a better translation, contributions are welcome.

## Translate on Weblate

Visit [Listen Moe on Weblate](https://hosted.weblate.org/projects/listenmoe/), sign in, and choose your language to translate in your browser. You can also reach the project from **About → Translate** in the app or the one-time translation invitation.

New translations are machine-generated drafts and need review by native speakers. Corrections to the app and its Flathub description are welcome.


To validate a catalog, run `msgfmt --check po/<locale>.po -o /tmp/listenmoe.mo`. To use translations with `cargo run`, compile them first with `sh .appimage-po.sh data/locale`.

## What to edit

Each `.po` file contains entries like:

```text
msgid "Original text"
msgstr "Translated text"
```


3. GitHub will ask you to fork the repository, accept it
4. Improve the `msgstr` text

## Suggest a correction

If you do not want to edit files on GitHub, you can open an [issue](https://github.com/noobping/listenmoe/issues) instead. In the issue, include the original text (`msgid`), your suggested translation (`msgstr`), and the language file name (for example `ja.po`).

## Guidelines

Keep the meaning of the original text the same. Do not add or remove information unless necessary for the language. Placeholders such as `%s`, `{name}`, or `%(count)d` must remain exactly the same. They may be moved if required by grammar, but must not be changed or removed. When editing files manually, leave `msgid` and unrelated comments or headers unchanged. Weblate maintains translation headers automatically. Translation credits and review annotations may be updated to reflect your contribution.
