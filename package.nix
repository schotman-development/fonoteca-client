# Written the way nixpkgs expects, so it can be sent there as it is. flake.nix
# calls it with the working tree as `src`; the fetched source is for nixpkgs.
{
  lib,
  rustPlatform,
  fetchFromGitHub,
  pkg-config,
  alsa-lib,
  fontconfig,
  pipewire,
  ffmpeg_7-headless,
  libGL,
  libxkbcommon,
  wayland,
  libxcb,
  libx11,
  libxcursor,
  libxi,
  libxrender,
  src ? fetchFromGitHub {
    owner = "schotman-development";
    repo = "fonoteca-client";
    rev = "77b024e9eb6df81bb0c5fa72a405f7e390bf1db4";
    hash = "sha256-R6sjNyxPJnDiXWyp1GZWB0t7bR8OCRpeFxR02ay8IrA=";
  },
}:

let
  appId = "io.github.schotman_development.fonoteca";
  # The FFmpeg the app is tested with, without its GPL parts.
  ffmpeg = ffmpeg_7-headless.override {
    withGPL = false;
    withVersion3 = false;
  };
  # Loaded by the window system at run time rather than linked.
  windowing = [
    libGL
    libxkbcommon
    wayland
    libxcb
    libx11
    libxcursor
    libxi
    libxrender
  ];
in
rustPlatform.buildRustPackage {
  pname = "fonoteca-client";
  version = "0.0.0";

  inherit src;

  cargoLock.lockFile = "${src}/Cargo.lock";

  cargoBuildFlags = [
    "-p"
    "fonoteca-client"
    "--bin"
    "fonoteca-client"
  ];

  nativeBuildInputs = [
    pkg-config
    rustPlatform.bindgenHook
  ];

  buildInputs = [
    ffmpeg
    alsa-lib
    fontconfig
    pipewire
  ];

  # The tests need a music server and a sound card; they run in development.
  doCheck = false;

  postInstall = ''
    install -Dm644 packaging/linux/${appId}.desktop -t $out/share/applications
    install -Dm644 packaging/linux/${appId}.metainfo.xml -t $out/share/metainfo
    mkdir -p $out/share/icons
    cp -r packaging/linux/hicolor $out/share/icons/
  '';

  postFixup = ''
    patchelf --add-rpath ${lib.makeLibraryPath windowing} $out/bin/fonoteca-client
  '';

  meta = {
    description = "A music player for OpenSubsonic servers";
    homepage = "https://github.com/schotman-development/fonoteca-client";
    license = lib.licenses.mit;
    mainProgram = "fonoteca-client";
    platforms = lib.platforms.linux;
    maintainers = [
      {
        name = "Chris";
        github = "SchotjeChrisman";
        githubId = 213425916;
      }
    ];
  };
}
