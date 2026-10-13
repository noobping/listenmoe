#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."

# Build and bundle on the same distribution so GTK, glibc and plugins agree.
arch=$(uname -m)
case "$arch" in
  x86_64) loader_name=ld-linux-x86-64.so.2 ;;
  aarch64) loader_name=ld-linux-aarch64.so.1 ;;
  *) echo "Unsupported architecture: $arch" >&2; exit 1 ;;
esac
version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["package"]["version"])')
version=${APPIMAGE_VERSION:-$version}
app_id=io.github.noobping.listenmoe
appdir="$PWD/AppDir"
tools_dir="$PWD/target/appimage-tools"
mkdir -p "$tools_dir"

cargo build --release
rm -rf "$appdir"
mkdir -p "$appdir/usr/"{bin,lib,share/applications,share/metainfo,share/glib-2.0/schemas}
install -m755 target/release/listenmoe "$appdir/usr/bin/"
install -m755 build-aux/appimage/AppRun "$appdir/AppRun"
install -m644 data/desktop.in "$appdir/usr/share/applications/$app_id.desktop"
ln -s "usr/share/applications/$app_id.desktop" "$appdir/$app_id.desktop"
install -m644 data/metainfo.xml.in "$appdir/usr/share/metainfo/$app_id.appdata.xml"
for size in 128x128 scalable symbolic; do
  mkdir -p "$appdir/usr/share/icons/hicolor/$size"
  cp -a "data/$size/apps" "$appdir/usr/share/icons/hicolor/$size/"
done
ln -s "usr/share/icons/hicolor/scalable/apps/$app_id.svg" "$appdir/$app_id.svg"
ln -s "$app_id.svg" "$appdir/.DirIcon"
install -m644 "data/$app_id.gschema.xml" "$appdir/usr/share/glib-2.0/schemas/"
cp /usr/share/glib-2.0/schemas/*.xml "$appdir/usr/share/glib-2.0/schemas/"
glib-compile-schemas --strict "$appdir/usr/share/glib-2.0/schemas"
./.appimage-po.sh

# Include modules loaded with dlopen, which ldd alone cannot discover.
multiarch=$(gcc -print-multiarch)
for module in gio/modules alsa-lib; do
  mkdir -p "$appdir/usr/lib/$module"
  cp -a /usr/lib/"$multiarch"/"$module"/. "$appdir/usr/lib/$module/"
done
mkdir -p "$appdir/usr/lib/gdk-pixbuf-loaders"
cp -a /usr/lib/"$multiarch"/gdk-pixbuf-2.0/2.10.0/loaders/*.so "$appdir/usr/lib/gdk-pixbuf-loaders/"
install -m755 /usr/lib/"$multiarch"/gdk-pixbuf-2.0/gdk-pixbuf-query-loaders "$appdir/usr/bin/"
cp -a /usr/share/alsa "$appdir/usr/share/"
install -m644 build-aux/appimage/alsa.conf "$appdir/usr/share/alsa/listenmoe.conf"
test -f "$appdir/usr/lib/alsa-lib/libasound_module_pcm_pulse.so"
cp -a /usr/share/fontconfig "$appdir/usr/share/"
cp -a /usr/share/mime "$appdir/usr/share/"
mkdir -p "$appdir/etc"
cp -rL /etc/fonts "$appdir/etc/"
mkdir -p "$appdir/etc/ssl/certs"
install -m644 /etc/ssl/certs/ca-certificates.crt "$appdir/etc/ssl/certs/"

# Copy the complete ELF dependency closure, including the real glibc loader.
# Dereference symlinks: absolute distro symlinks must not escape the bundle.
python3 build-aux/appimage/bundle-libraries.py "$appdir"
test -f "$appdir/usr/lib/$loader_name"

# appimagetool 1.9.1 creates a type-2 image with the modern static FUSE runtime.
tool="$tools_dir/appimagetool-$arch.AppImage"
if [[ ! -f "$tool" ]]; then
  curl --fail --location --retry 3 \
    "https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-$arch.AppImage" \
    -o "$tool"
  chmod +x "$tool"
fi
output="$PWD/listenmoe-$version-$arch.AppImage"
ARCH="$arch" VERSION="$version" APPIMAGE_EXTRACT_AND_RUN=1 "$tool" \
  --comp zstd \
  --updateinformation "gh-releases-zsync|noobping|listenmoe|latest|listenmoe-*-$arch.AppImage.zsync" \
  "$appdir" "$output"
sha256sum "${output##*/}" > "$output.sha256"
printf 'Built %s\n' "$output"
