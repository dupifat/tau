{
  description = "tau";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    flake-utils.url = "github:numtide/flake-utils";
    flakebox.url = "github:rustshop/flakebox";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      flakebox,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        projectName = "tau";

        flakeboxLib = flakebox.lib.mkLib pkgs {
          config = {
            github.ci.buildOutputs = [ ".#ci.workspace" ];
            just.importPaths = [ "justfile.custom.just" ];
            just.rules.watch.enable = false;
          };
        };

        buildPaths = [
          "Cargo.toml"
          "Cargo.lock"
          "config"
          "crates"
        ];

        buildSrc = flakeboxLib.filterSubPaths {
          root = builtins.path {
            name = projectName;
            path = ./.;
          };
          paths = buildPaths;
        };

        tauBuildPlaceholder = "01234569abcdef7afa1d2683a099c7af48a523c1";
        tauBuildDatePlaceholder = "1970-01-01 00:00";
        tauBuildRevision =
          if (self ? rev) && (builtins.stringLength self.rev == 40) then
            self.rev
          else if (self ? dirtyRev) && (builtins.stringLength self.dirtyRev == 46) then
            "${builtins.substring 0 16 self.dirtyRev}00000000${builtins.substring 24 16 self.dirtyRev}"
          else if (self ? dirtyRev) && (builtins.stringLength self.dirtyRev == 40) then
            self.dirtyRev
          else
            tauBuildPlaceholder;
        tauBuildShortRevision = builtins.substring 0 7 tauBuildRevision;
        tauBuildDate =
          if self ? lastModifiedDate then
            "${builtins.substring 0 4 self.lastModifiedDate}-${builtins.substring 4 2 self.lastModifiedDate}-${
              builtins.substring 6 2 self.lastModifiedDate
            } ${builtins.substring 8 2 self.lastModifiedDate}:${builtins.substring 10 2 self.lastModifiedDate}"
          else
            tauBuildDatePlaceholder;

        replaceTauBuildInfo =
          package:
          pkgs.stdenv.mkDerivation {
            pname = projectName;
            version = package.version;

            dontUnpack = true;
            dontStrip = true;

            nativeBuildInputs = [ pkgs.bbe ];

            installPhase = ''
              cp -a ${package} $out
              chmod -R u+w $out
              for path in $(${pkgs.findutils}/bin/find $out -type f -executable); do
                ${pkgs.bbe}/bin/bbe \
                  -e 's/${builtins.substring 0 7 tauBuildPlaceholder}/${tauBuildShortRevision}/' \
                  -e 's/${tauBuildDatePlaceholder}/${tauBuildDate}/' \
                  "$path" -o ./tmp
                cat ./tmp > "$path"
              done
            '';
          };

        multiBuild = (flakeboxLib.craneMultiBuild { }) (
          craneLib':
          let
            craneLib = craneLib'.overrideArgs {
              pname = projectName;
              src = buildSrc;
              nativeBuildInputs = [ ];
              BUILT_OVERRIDE_tau-cli_GIT_COMMIT_HASH = tauBuildPlaceholder;
              BUILT_OVERRIDE_tau-cli_GIT_COMMIT_HASH_SHORT = builtins.substring 0 7 tauBuildPlaceholder;
              TAU_LAST_MODIFIED = tauBuildDatePlaceholder;
            };
          in
          rec {
            workspaceDeps = craneLib.buildWorkspaceDepsOnly { };

            workspace = craneLib.buildWorkspace {
              cargoArtifacts = workspaceDeps;
            };

            tests = craneLib.cargoNextest {
              cargoArtifacts = workspace;
              cargoNextestExtraArgs = "--no-tests=pass";
            };

            clippy = craneLib.cargoClippy {
              cargoArtifacts = workspaceDeps;
            };

            tau = replaceTauBuildInfo (
              craneLib.buildPackage {
                cargoArtifacts = workspaceDeps;
                cargoExtraArgs = "-p tau";
              }
            );
          }
        );

        site = pkgs.runCommand "tau-agent-site" { } ''
          mkdir -p $out/share/tau-agent-site
          cp -r ${./site}/* $out/share/tau-agent-site/
        '';
      in
      {
        packages.default = multiBuild.tau;
        packages.tau = multiBuild.tau;
        packages.site = site;

        ci = {
          inherit (multiBuild) workspace clippy tests;
        };

        legacyPackages = multiBuild;

        devShells = flakeboxLib.mkShells {
          packages = [ ];
        };
      }
    );
}
