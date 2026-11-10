#!/bin/sh
# Builds what the Flatpak installs, into target/flatpak/stage, from the repo
# root as root in docker.io/library/rust:slim-bookworm: the binary, the LGPL
# FFmpeg libraries it links, and their licence notice. flatpak-builder builds
# offline and cannot fetch the crates, so the binary is built here and the
# manifest only installs it. The binary runs on the Flatpak runtime's newer
# glibc and on its PipeWire, ALSA, fontconfig and window-system libraries, which
# keep their sonames; only FFmpeg is not in the runtime.
set -eu
cd "$(dirname "$0")/.."
root=$PWD
stage=$root/target/flatpak/stage

apt-get update -qq
apt-get install -y -qq --no-install-recommends pkg-config libfontconfig-dev libasound2-dev libpipewire-0.3-dev \
  libclang-dev clang nasm xz-utils make zlib1g-dev gnupg curl ca-certificates >/dev/null

scripts/ffmpeg.sh linux
export FFMPEG_DIR=$root/target/linux-ffmpeg/prefix
cargo build --locked --release -p fonoteca-client --bin fonoteca-client

rm -rf "$stage" && mkdir -p "$stage/bin" "$stage/lib" "$stage/licenses"
cp target/release/fonoteca-client "$stage/bin/"
strip "$stage/bin/fonoteca-client"
cp -P "$FFMPEG_DIR"/lib/lib*.so* "$stage/lib/"
cp LICENSE target/linux-ffmpeg/notice/* "$stage/licenses/"
