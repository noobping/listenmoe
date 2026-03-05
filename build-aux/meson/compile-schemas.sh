#!/usr/bin/env sh
set -eu

schemas_rel_dir="$1"
schemas_dir="${MESON_INSTALL_DESTDIR_PREFIX}/${schemas_rel_dir}"

if [ -d "$schemas_dir" ]; then
  glib-compile-schemas "$schemas_dir"
fi
