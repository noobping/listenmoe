#!/bin/sh
set -eu
APP_ID=io.github.noobping.listenmoe
OUTDIR="${1:-AppDir/usr/share/locale}"

for f in po/*.po; do
    lang=$(basename "$f" .po)
    mkdir -p "$OUTDIR/$lang/LC_MESSAGES"
    msgfmt "$f" -o "$OUTDIR/$lang/LC_MESSAGES/$APP_ID.mo"
done
