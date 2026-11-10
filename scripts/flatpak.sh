#!/bin/sh
# Packs target/flatpak/stage, which scripts/flatpak-stage.sh builds, into
# target/Fonoteca-<version>-linux-x86_64.flatpak with flatpak-builder, from the
# repo root. Needs flatpak and flatpak-builder, and Flathub for the runtime.
set -eu
version=${1:?usage: $0 <version>}
cd "$(dirname "$0")/.."
work=target/flatpak
app_id=io.github.schotman_development.fonoteca

flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --force-clean --install-deps-from=flathub --disable-rofiles-fuse \
  --repo="$work/repo" "$work/build" packaging/linux/$app_id.yml
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo "$work/repo" "target/Fonoteca-$version-linux-x86_64.flatpak" $app_id
