#!/bin/sh
# Builds the LGPL FFmpeg the player links against, for Android arm64, macOS
# on Apple silicon or Linux on x86_64, with only the audio decoders, demuxers, parsers and
# filters it uses, as shared libraries in target/<platform>-ffmpeg/prefix
# (FFMPEG_DIR for cargo), with its licence and configure line in
# target/<platform>-ffmpeg/notice. For Android it copies the libraries into
# jniLibs and the notice into the assets, where Gradle packs them; Android
# needs ANDROID_NDK_HOME.
set -eu
platform=${1:-}
version=7.1.5
# FFmpeg's release signing key, so a tarball and key swapped together on the
# server still fail.
key=FCF986EA15E6E293A5644F10B4322F04D67658D8
cd "$(dirname "$0")/.."
main=$PWD/android/src/main
work=$PWD/target/$platform-ffmpeg
case $platform in
  android)
    tools=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin
    # Linked for Android 9 (API 28), the app's oldest, and for 16 KB pages.
    # --target-os=android names each library and its SONAME plain
    # libavcodec.so, without the version suffix Android's loader doesn't
    # look for.
    set -- --enable-cross-compile --target-os=android --arch=aarch64 \
      --cc="$tools/aarch64-linux-android28-clang" --ar="$tools/llvm-ar" --nm="$tools/llvm-nm" \
      --ranlib="$tools/llvm-ranlib" --strip="$tools/llvm-strip" \
      --extra-ldflags=-Wl,-z,max-page-size=16384
    ;;
  macos)
    # Found through the app's rpath, inside its bundle; for macOS 11, the
    # first on Apple silicon.
    set -- --install-name-dir=@rpath \
      --extra-cflags=-mmacosx-version-min=11.0 --extra-ldflags=-mmacosx-version-min=11.0
    ;;
  linux)
    # Plain shared libraries; the AppImage script sets where they look for
    # each other.
    set --
    ;;
  *)
    echo "usage: $0 android|macos|linux" >&2
    exit 2
    ;;
esac
mkdir -p "$work"
cd "$work"
# ffmpeg.org drops connections now and then.
for url in https://ffmpeg.org/ffmpeg-devel.asc https://ffmpeg.org/releases/ffmpeg-$version.tar.xz.asc https://ffmpeg.org/releases/ffmpeg-$version.tar.xz; do
  curl -fsSL --retry 5 --retry-all-errors -o "${url##*/}" "$url"
done
export GNUPGHOME="$work/gnupg"
rm -rf "$GNUPGHOME" && mkdir -m 700 "$GNUPGHOME"
gpg -q --import ffmpeg-devel.asc
gpg -q --status-fd 1 --verify ffmpeg-$version.tar.xz.asc ffmpeg-$version.tar.xz | grep -q "VALIDSIG .* $key$"
rm -rf ffmpeg-$version prefix notice && tar --no-same-owner -xf ffmpeg-$version.tar.xz
cd ffmpeg-$version
./configure --prefix="$work/prefix" --enable-shared --disable-static "$@" \
  --disable-programs --disable-doc --disable-autodetect --enable-zlib \
  --disable-avdevice --disable-swscale --disable-network --disable-everything \
  --enable-decoder=flac,alac,mp3,mp3float,aac,aac_latm,vorbis,opus,wavpack,ape,tta,wmav1,wmav2,wmalossless,dsd_lsbf,dsd_msbf,dsd_lsbf_planar,dsd_msbf_planar,pcm_s16le,pcm_s16be,pcm_s24le,pcm_s24be,pcm_s32le,pcm_s32be,pcm_f32le,pcm_f32be,pcm_f64le,pcm_f64be,pcm_u8 \
  --enable-demuxer=flac,mov,mp3,ogg,wav,w64,aiff,wv,ape,tta,matroska,dsf,iff,asf,aac,mpegts \
  --enable-parser=flac,mpegaudio,aac,aac_latm,vorbis,opus \
  --enable-filter=abuffer,abuffersink,aresample,aformat,volume,anull,equalizer,lowshelf,highshelf,lowpass,highpass,crossfeed \
  > "$work/configure.log"
grep -q "License: LGPL version 2.1 or later" "$work/configure.log"
make -j"$(getconf _NPROCESSORS_ONLN)" >/dev/null
make install >/dev/null
mkdir -p "$work/notice"
cp COPYING.LGPLv2.1 "$work/notice/"
{
  echo "FFmpeg $version under the LGPL version 2.1 or later (COPYING.LGPLv2.1), from https://ffmpeg.org/releases/ffmpeg-$version.tar.xz, configured with:"
  grep FFMPEG_CONFIGURATION config.h
} > "$work/notice/ffmpeg-configure.txt"
if [ "$platform" = android ]; then
  mkdir -p "$main/jniLibs/arm64-v8a" "$main/assets/ffmpeg"
  cp "$work"/prefix/lib/lib*.so "$main/jniLibs/arm64-v8a/"
  cp "$work"/notice/* "$main/assets/ffmpeg/"
fi
