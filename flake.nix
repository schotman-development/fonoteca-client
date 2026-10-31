{
  description = "A music player for OpenSubsonic servers";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs =
    { nixpkgs, ... }:
    let
      forEachSystem = nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
      ];
    in
    {
      packages = forEachSystem (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          inherit (pkgs) lib;
          # The FFmpeg the app is tested with, without its GPL parts.
          ffmpeg = pkgs.ffmpeg_7-headless.override {
            withGPL = false;
            withVersion3 = false;
          };
          # Loaded by the window system at run time rather than linked.
          windowing = with pkgs; [
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
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "fonoteca-client";
            version = "0.0.0";
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./crates
                ./packaging/linux
              ];
            };
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "-p"
              "fonoteca-client"
              "--bin"
              "fonoteca-client"
            ];
            nativeBuildInputs = [
              pkgs.pkg-config
              pkgs.rustPlatform.bindgenHook
            ];
            buildInputs = [
              ffmpeg
              pkgs.alsa-lib
              pkgs.fontconfig
            ];
            # The tests need a music server and a sound card; they run in
            # development.
            doCheck = false;
            postFixup = ''
              patchelf --add-rpath ${lib.makeLibraryPath windowing} $out/bin/fonoteca-client
            '';
            meta = {
              description = "A music player for OpenSubsonic servers";
              license = lib.licenses.mit;
              mainProgram = "fonoteca-client";
            };
          };
        }
      );
    };
}
