project(
  'listenmoe',
  'c',
  version: '1.1.0',
  meson_version: '>= 0.62.0',
)

cargo = find_program('cargo', required: true)
sh = find_program('sh', required: true)
gnome = import('gnome')

dependency('gtk4', version: '>= 4.0', required: true)
dependency('libadwaita-1', version: '>= 1.5', required: true)
dependency('gdk-pixbuf-2.0', required: true)
dependency('glib-2.0', required: true)

cargo_profile = get_option('cargo_profile')
cargo_features = get_option('cargo_features')
cargo_default_features = get_option('cargo_default_features')

features_csv = ''
if cargo_features.length() > 0
  features_csv = ','.join(cargo_features)
endif

cargo_build_script = '''
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
'''

listenmoe_bin = custom_target(
  'listenmoe-bin',
  output: 'listenmoe',
  command: [
    sh,
    '-eu',
    '-c',
    cargo_build_script,
    'cargo-build.sh',
    meson.project_source_root(),
    meson.project_build_root(),
    cargo,
    cargo_profile,
    cargo_default_features ? 'true' : 'false',
    features_csv,
    '@OUTPUT@',
  ],
  build_by_default: true,
  install: true,
  install_dir: get_option('bindir'),
  console: true,
)

cargo_run_cmd = [
  cargo,
  'run',
  '--manifest-path',
  join_paths(meson.project_source_root(), 'Cargo.toml'),
]
if cargo_profile == 'release'
  cargo_run_cmd += ['--release']
endif
if not cargo_default_features
  cargo_run_cmd += ['--no-default-features']
endif
if features_csv != ''
  cargo_run_cmd += ['--features', features_csv]
endif
run_target('cargo-run', command: cargo_run_cmd)
run_target('run', command: [listenmoe_bin])

desktop_file = configure_file(
  input: 'data/desktop.in',
  output: 'io.github.noobping.listenmoe.desktop',
  copy: true,
)
install_data(
  desktop_file,
  install_dir: join_paths(get_option('datadir'), 'applications'),
)

metainfo_file = configure_file(
  input: 'data/metainfo.xml.in',
  output: 'io.github.noobping.listenmoe.metainfo.xml',
  copy: true,
)
install_data(
  metainfo_file,
  install_dir: join_paths(get_option('datadir'), 'metainfo'),
)

install_data(
  'data/io.github.noobping.listenmoe.gschema.xml',
  install_dir: join_paths(get_option('datadir'), 'glib-2.0', 'schemas'),
)

install_data(
  'data/scalable/apps/io.github.noobping.listenmoe.svg',
  install_dir: join_paths(get_option('datadir'), 'icons', 'hicolor', 'scalable', 'apps'),
)
devel_svg_icon = configure_file(
  input: 'data/scalable/apps/io.github.noobping.listenmoe_beta.svg',
  output: 'io.github.noobping.listenmoe.Devel.svg',
  copy: true,
)
install_data(
  devel_svg_icon,
  install_dir: join_paths(get_option('datadir'), 'icons', 'hicolor', 'scalable', 'apps'),
)
install_data(
  'data/128x128/apps/io.github.noobping.listenmoe.png',
  install_dir: join_paths(get_option('datadir'), 'icons', 'hicolor', '128x128', 'apps'),
)
devel_png_icon = configure_file(
  input: 'data/128x128/apps/io.github.noobping.listenmoe_beta.png',
  output: 'io.github.noobping.listenmoe.Devel.png',
  copy: true,
)
install_data(
  devel_png_icon,
  install_dir: join_paths(get_option('datadir'), 'icons', 'hicolor', '128x128', 'apps'),
)
install_subdir(
  'data/symbolic/apps',
  install_dir: join_paths(get_option('datadir'), 'icons', 'hicolor', 'symbolic'),
)

gnome.post_install(
  glib_compile_schemas: true,
)

meson.add_install_script(
  sh,
  '-eu',
  '-c',
  '''
schemas_rel_dir="$1"
schemas_dir="${MESON_INSTALL_DESTDIR_PREFIX}/${schemas_rel_dir}"

if [ -d "$schemas_dir" ]; then
  glib-compile-schemas "$schemas_dir"
fi
''',
  'compile-schemas.sh',
  join_paths(get_option('datadir'), 'glib-2.0', 'schemas'),
)
