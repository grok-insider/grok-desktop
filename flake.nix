{
  description = "Grok Desktop development and verification environment";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          default = pkgs.mkShell {
            strictDeps = true;
            packages = with pkgs; [
              actionlint
              buf
              cargo
              cargo-deny
              check-jsonschema
              clippy
              cmake
              go
              govulncheck
              gnumake
              jq
              appimageupdate
              nixfmt
              nodejs_22
              corepack_22
              openssl
              pinentry-qt
              pkg-config
              protobuf
              protoc-gen-go
              rustc
              rustfmt
              shellcheck
              sqlcipher
            ];

            shellHook = ''
              export GROK_DESKTOP_ROOT="$PWD"
              export RUST_BACKTRACE=1
              # corepack shims (pnpm/yarn) resolve the package.json packageManager
              # pin; skip the interactive download prompt on first use.
              export COREPACK_ENABLE_DOWNLOAD_PROMPT=0
              # The daemon's unix credential prompt speaks Assuan to pinentry;
              # point it at the Qt flavor explicitly (the package ships no
              # plain `pinentry` binary).
              export GROK_PINENTRY=${pkgs.pinentry-qt}/bin/pinentry-qt
              # The npm-pinned Electron is an FHS binary; on NixOS it runs via
              # nix-ld, which needs Chromium's runtime libraries on this path.
              # (nixpkgs' own electron_42 SIGILLs on kernel 7.x — the pinned
              # 43.1.0 binary is the one that works, so keep it authoritative.)
              export NIX_LD_LIBRARY_PATH="${
                pkgs.lib.makeLibraryPath (
                  with pkgs;
                  [
                    alsa-lib
                    at-spi2-atk
                    at-spi2-core
                    atk
                    cairo
                    cups
                    dbus
                    expat
                    fontconfig
                    freetype
                    gdk-pixbuf
                    glib
                    gtk3
                    libdrm
                    libgbm
                    libglvnd
                    libxkbcommon
                    mesa
                    nspr
                    nss
                    pango
                    pciutils
                    stdenv.cc.cc.lib
                    systemd
                    vulkan-loader
                    wayland
                    xorg.libX11
                    xorg.libXcomposite
                    xorg.libXcursor
                    xorg.libXdamage
                    xorg.libXext
                    xorg.libXfixes
                    xorg.libXi
                    xorg.libXrandr
                    xorg.libXrender
                    xorg.libXtst
                    xorg.libxcb
                    xorg.libxshmfence
                  ]
                )
              }''${NIX_LD_LIBRARY_PATH:+:$NIX_LD_LIBRARY_PATH}"
            '';
          };
        }
      );

      formatter = forAllSystems (system: (import nixpkgs { inherit system; }).nixfmt);

      packages =
        nixpkgs.lib.genAttrs
          [
            "x86_64-linux"
            "aarch64-linux"
          ]
          (
            system:
            let
              pkgs = import nixpkgs { inherit system; };
              guestRunner = pkgs.buildGoModule {
                pname = "grok-integration-runner";
                version = "0.0.3";
                src = self;
                modRoot = "guest/runner";
                subPackages = [
                  "cmd/grok-integration-runner"
                  "cmd/grok-workspace-mounter"
                ];
                vendorHash = "sha256-QvktaGD/phuMlPA6MiKpXIDFHUzWLU4LCgViuspyXg0=";

                postInstall = ''
                  				  install -Dm0444 ${self}/integrations/schema/computer-use-v1.schema.json \
                  				    $out/share/grok-desktop/schemas/computer-use-v1.schema.json
                  				'';

                meta = {
                  description = "Host-only managed integration supervisor for the Grok Desktop utility guest";
                  license = pkgs.lib.licenses.agpl3Plus;
                  mainProgram = "grok-integration-runner";
                  platforms = pkgs.lib.platforms.linux;
                };
              };
              guestSystem = nixpkgs.lib.nixosSystem {
                inherit system;
                modules = [
                  ./guest/nixos/configuration.nix
                  {
                    grok.guest.integrationRunner = {
                      enable = true;
                      package = guestRunner;
                    };
                  }
                ];
                specialArgs.imageVersion = self.shortRev or self.dirtyShortRev or "development";
              };
            in
            {
              inherit guestRunner;
              hypervImage = guestSystem.config.system.build.images.hyperv;
              guestModuleEvaluation =
                assert
                  guestSystem.config.systemd.services.grok-integration-runner.serviceConfig.IPAddressDeny == "any";
                assert builtins.elem "CAP_CHOWN"
                  guestSystem.config.systemd.services.grok-workspace-mounter.serviceConfig.CapabilityBoundingSet;
                pkgs.runCommandLocal "grok-runner-module-evaluation" { } "touch $out";
              default = guestRunner;
            }
          );

      checks = nixpkgs.lib.mapAttrs (system: packages: {
        guest-runner = packages.guestRunner;
        guest-module-evaluation = packages.guestModuleEvaluation;
      }) self.packages;
    };
}
