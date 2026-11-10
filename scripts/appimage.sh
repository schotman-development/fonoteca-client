#!/bin/sh
# Builds the Linux x86_64 AppImage, target/Fonoteca-<version>-linux-x86_64.AppImage,
# from the repo root as root in docker.io/library/rust:slim-bookworm. Bookworm's
# glibc 2.36 and PipeWire 0.3.65 set what the AppImage runs on: any distribution
# with glibc 2.36 or newer. It carries FFmpeg, PipeWire, ALSA, fontconfig and
# the window-system libraries the app loads at run time; the graphics driver
# (libGL, libEGL) and the D-Bus session for the Secret Service are the host's.
set -eu
version=${1:?usage: $0 <version>}
# appimagetool and the runtime it writes at the front of the AppImage, pinned
# by checksum.
tool_url=https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage
tool_sha256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
runtime_url=https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64
runtime_sha256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
# Libraries the app opens by name at run time rather than links to.
loaded_at_run_time="libwayland-client0 libwayland-cursor0 libwayland-egl1 libxkbcommon0 libxkbcommon-x11-0
  libx11-6 libx11-xcb1 libxcb1 libxcursor1 libxext6 libxfixes3 libxi6 libxrandr2 libxrender1"
# What every distribution has, or the graphics driver brings.
host_libraries='^(libc|libm|libdl|libpthread|librt|libresolv|libutil|libgcc_s|ld-linux-x86-64|libGL|libEGL|libGLdispatch|libGLX|libgbm|libdrm)\.so'

cd "$(dirname "$0")/.."
root=$PWD
work=$root/target/appimage
appdir=$work/AppDir

apt-get update -qq
apt-get install -y -qq --no-install-recommends pkg-config libfontconfig-dev libasound2-dev libpipewire-0.3-dev \
  libpipewire-0.3-modules pipewire-bin libclang-dev clang nasm xz-utils make zlib1g-dev gnupg curl ca-certificates patchelf file $loaded_at_run_time >/dev/null

scripts/ffmpeg.sh linux
export FFMPEG_DIR=$root/target/linux-ffmpeg/prefix
# Beside the binary's own libraries, in usr/lib.
export RUSTFLAGS='-C link-arg=-Wl,-rpath,$ORIGIN/../lib'
cargo build --locked --release -p fonoteca-client --bin fonoteca-client

rm -rf "$appdir" && mkdir -p "$appdir/usr/bin" "$appdir/usr/lib" "$appdir/usr/share"
cp target/release/fonoteca-client "$appdir/usr/bin/"
strip "$appdir/usr/bin/fonoteca-client"

# Copies the libraries a file links, and the files given as libraries, into $1,
# but not what the host brings. Every copy looks for the others in usr/lib and
# beside itself, as the binary's rpath reaches only its own libraries.
bundle() {
  destination=$1; shift
  links() { LD_LIBRARY_PATH=$FFMPEG_DIR/lib ldd "$1" | awk '$3 ~ /^\// { print $3 }'; }
  for file in $(for f in "$@"; do links "$f"; done; for f in "$@"; do case $f in */fonoteca-client) ;; *) echo "$f" ;; esac; done); do
    echo "${file##*/}" | grep -Eq "$host_libraries" && continue
    [ "$destination" != "$appdir/usr/lib" ] && [ -e "$appdir/usr/lib/${file##*/}" ] && continue
    cp -L "$file" "$destination/"
    patchelf --set-rpath "\$ORIGIN:\$ORIGIN/$(realpath --relative-to="$destination" "$appdir/usr/lib")" "$destination/${file##*/}"
  done
}

# What the binary links, and the libraries it opens by name later.
loaded=$(for package in $loaded_at_run_time; do dpkg -L "$package" | grep -E '/lib[^/]*\.so\.[0-9]+$'; done)
bundle "$appdir/usr/lib" "$appdir/usr/bin/fonoteca-client" $loaded
test -f "$appdir/usr/lib/libavcodec.so.61"

# PipeWire loads its protocol and node modules from directories fixed when it
# was built, and its configuration from /usr/share; AppRun points it at these.
# Only what a client uses: the others want D-Bus and more.
multiarch=$(gcc -dumpmachine)
modules=/usr/lib/$multiarch/pipewire-0.3
plugins=/usr/lib/$multiarch/spa-0.2
mkdir -p "$appdir/usr/lib/pipewire-0.3" "$appdir/usr/lib/spa-0.2/support" "$appdir/usr/lib/spa-0.2/audioconvert" "$appdir/usr/share/pipewire"
for module in protocol-native client-node client-device adapter metadata session-manager; do
  bundle "$appdir/usr/lib/pipewire-0.3" "$modules/libpipewire-module-$module.so"
done
bundle "$appdir/usr/lib/spa-0.2/support" "$plugins/support/libspa-support.so"
bundle "$appdir/usr/lib/spa-0.2/audioconvert" "$plugins/audioconvert/libspa-audioconvert.so"
cp /usr/share/pipewire/client.conf "$appdir/usr/share/pipewire/"

licenses=$appdir/usr/share/licenses/fonoteca
mkdir -p "$licenses"
cp LICENSE "$licenses/"
cp target/linux-ffmpeg/notice/* "$licenses/"

cp packaging/linux/io.github.schotman_development.fonoteca.desktop "$appdir/"
mkdir -p "$appdir/usr/share/metainfo"
cp packaging/linux/io.github.schotman_development.fonoteca.metainfo.xml "$appdir/usr/share/metainfo/"
cp -r packaging/linux/hicolor "$appdir/usr/share/icons"
cp packaging/linux/hicolor/256x256/apps/io.github.schotman_development.fonoteca.png "$appdir/"
ln -s io.github.schotman_development.fonoteca.png "$appdir/.DirIcon"
cat > "$appdir/AppRun" <<'RUN'
#!/bin/sh
here=$(dirname "$(readlink -f "$0")")
export PIPEWIRE_MODULE_DIR=$here/usr/lib/pipewire-0.3
export SPA_PLUGIN_DIR=$here/usr/lib/spa-0.2
export PIPEWIRE_CONFIG_DIR=$here/usr/share/pipewire
exec "$here/usr/bin/fonoteca-client" "$@"
RUN
chmod +x "$appdir/AppRun"

# No FUSE in a container: the tool runs from its unpacked files.
mkdir -p "$work/tools"
download() { curl -fsSL --retry 5 --retry-all-errors -o "$work/tools/$1" "$2" && echo "$3  $work/tools/$1" | sha256sum -c - >/dev/null; }
download appimagetool "$tool_url" "$tool_sha256"
download runtime "$runtime_url" "$runtime_sha256"
chmod +x "$work/tools/appimagetool"
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$work/tools/appimagetool" --runtime-file "$work/tools/runtime" \
  "$appdir" "$root/target/Fonoteca-$version-linux-x86_64.AppImage"
