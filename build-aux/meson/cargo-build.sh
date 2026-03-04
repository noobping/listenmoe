#!/usr/bin/env sh
set -eu

src_root="$1"
build_root="$2"
cargo_bin="$3"
profile="$4"
default_features="$5"
features_csv="$6"
output_bin="$7"

manifest="$src_root/Cargo.toml"
target_dir="$build_root/target"
bin_name="listenmoe"

set -- "$cargo_bin" build --manifest-path "$manifest" --target-dir "$target_dir"

if [ "$profile" = "release" ]; then
  set -- "$@" --release
fi

if [ "$default_features" != "true" ]; then
  set -- "$@" --no-default-features
fi

if [ -n "$features_csv" ]; then
  set -- "$@" --features "$features_csv"
fi

"$@"

if [ "$profile" = "release" ]; then
  built_bin="$target_dir/release/$bin_name"
else
  built_bin="$target_dir/debug/$bin_name"
fi

cp "$built_bin" "$output_bin"
