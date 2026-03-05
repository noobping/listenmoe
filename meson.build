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

cargo_build_script = files('build-aux/meson/cargo-build.sh')
compile_schemas_script = files('build-aux/meson/compile-schemas.sh')

listenmoe_bin = custom_target(
  'listenmoe-bin',
  output: 'listenmoe',
  command: [
    sh,
    cargo_build_script,
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
install_data(
  'data/scalable/apps/io.github.noobping.listenmoe.Devel.svg',
  install_dir: join_paths(get_option('datadir'), 'icons', 'hicolor', 'scalable', 'apps'),
)
install_data(
  'data/128x128/apps/io.github.noobping.listenmoe.png',
  install_dir: join_paths(get_option('datadir'), 'icons', 'hicolor', '128x128', 'apps'),
)
install_data(
  'data/128x128/apps/io.github.noobping.listenmoe.Devel.png',
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
  compile_schemas_script,
  join_paths(get_option('datadir'), 'glib-2.0', 'schemas'),
)
