{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.grok.guest;
  computerUseSchemaPath = "/etc/grok-desktop/schemas/computer-use-v1.schema.json";
  policy = {
    version = 1;
    imageVersion = cfg.imageVersion;
    manifestRoots = [ "/etc/grok-desktop/integrations" ];
    trustedManifestKeyFiles = map toString cfg.trustedManifestKeyFiles;
    publisherTrust = cfg.publisherTrust;
    unsignedDevelopmentPublishers = [ ];
    workspaceRoot = cfg.workspaceRoot;
    stateRoot = cfg.stateRoot;
    allowUnsignedDevelopment = false;
    maxMessageBytes = cfg.maxMessageBytes;
    controlPort = 4050;
    bundleOwnerUid = 0;
    bubblewrapPath = "${pkgs.bubblewrap}/bin/bwrap";
    computerUseSchema = computerUseSchemaPath;
    workspaceMounterSocket = "/run/grok-desktop/workspace-mounter.sock";
    transport = {
      family = "AF_VSOCK";
      purpose = "control";
    };
  };
in
{
  options.grok.guest = {
    enable = lib.mkEnableOption "the Grok Desktop utility guest baseline";

    imageVersion = lib.mkOption {
      type = lib.types.str;
      default = "development";
      description = "Immutable image identity reported to the Windows service.";
    };

    workspaceRoot = lib.mkOption {
      type = lib.types.str;
      default = "/run/grok-desktop/workspaces";
      readOnly = true;
      description = "Root for service-provided read-only workspace mounts.";
    };

    stateRoot = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/grok-integrations";
      readOnly = true;
      description = "Guest-only integration state root.";
    };

    trustedManifestKeyFiles = lib.mkOption {
      type = lib.types.listOf lib.types.path;
      default = [ ];
      description = "Ed25519 public keys accepted for managed bundles.";
    };

    trustedCertificateFiles = lib.mkOption {
      type = lib.types.listOf lib.types.path;
      default = [ ];
      description = "Optional enterprise CA files baked into a qualified image.";
    };

    publisherTrust = lib.mkOption {
      type = lib.types.attrsOf (
        lib.types.enum [
          "first-party"
          "third-party"
        ]
      );
      default = { };
      description = "Publisher identities and trust classes admitted by the signed catalog.";
    };

    integrationBundleDirectory = lib.mkOption {
      type = lib.types.path;
      default = pkgs.runCommandLocal "empty-grok-integration-bundles" { } "mkdir -p $out";
      description = "Immutable directory containing catalog-addressable integration bundles.";
    };

    maxMessageBytes = lib.mkOption {
      type = lib.types.ints.between 4096 16777216;
      # Leaves 64 KiB for the authenticated named-pipe and service envelopes.
      default = 8323072;
      description = "Maximum control or adapter protocol message size.";
    };

    integrationRunner = {
      enable = lib.mkEnableOption "the managed integration runner";
      package = lib.mkOption {
        type = lib.types.nullOr lib.types.package;
        default = null;
        description = "Package containing bin/grok-integration-runner.";
      };
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        assertions = [
          {
            assertion = (!cfg.integrationRunner.enable) || cfg.integrationRunner.package != null;
            message = "grok.guest.integrationRunner.package is required when the runner is enabled";
          }
        ];

        environment.etc = {
          "grok-desktop/image-version".text = "${cfg.imageVersion}\n";
          "grok-desktop/policy.json" = {
            mode = "0444";
            text = builtins.toJSON policy;
          };
          "grok-desktop/integrations".source = cfg.integrationBundleDirectory;
          "grok/requirements.toml" = {
            mode = "0444";
            text = ''
                            			  # Managed by Grok Desktop. This root-owned policy is authoritative.
                            			  [grok_com_config]
                            			  disable_api_key_auth = true

                            			  [ui]
                            			  disable_bypass_permissions_mode = true

                            			  [models]
                            			  default = "grok-build"

                            			  [cli]
                            			  auto_update = false

                            			  [sandbox]
                            			  profile = "strict"

                            			  [skills]
                            			  paths = []

                            [plugins]
                            paths = []

              			  [compat.cursor]
              			  skills = false
              			  rules = false
              			  agents = false
              			  mcps = false
              			  hooks = false

              			  [compat.claude]
              			  skills = false
              			  rules = false
              			  agents = false
              			  mcps = false
              			  hooks = false
            '';
          };
        };

        security.pki.certificateFiles = cfg.trustedCertificateFiles;

        users.groups.grok-integrations = { };
        users.users.grok-integrations = {
          isSystemUser = true;
          group = "grok-integrations";
          home = cfg.stateRoot;
          createHome = false;
          shell = pkgs.shadow + "/bin/nologin";
        };

        systemd.tmpfiles.rules = [
          "d /run/grok-desktop 0750 root grok-integrations - -"
          "d ${cfg.workspaceRoot} 0550 root grok-integrations - -"
          "d ${cfg.stateRoot} 0700 grok-integrations grok-integrations - -"
        ];

        # Do not expose a recovery login on the VM console.
        systemd.services."getty@tty1".enable = false;
        systemd.services."autovt@tty1".enable = false;
      }

      (lib.mkIf cfg.integrationRunner.enable {
        environment.etc."grok-desktop/schemas/computer-use-v1.schema.json".source =
          cfg.integrationRunner.package + "/share/grok-desktop/schemas/computer-use-v1.schema.json";

        systemd.services.grok-workspace-mounter = {
          description = "Grok Desktop read-only workspace mount broker";
          wantedBy = [ "multi-user.target" ];
          before = [ "grok-integration-runner.service" ];
          after = [ "systemd-tmpfiles-setup.service" ];
          serviceConfig = {
            Type = "notify";
            ExecStart = "${lib.getExe' cfg.integrationRunner.package "grok-workspace-mounter"} --policy /etc/grok-desktop/policy.json --runner-user grok-integrations --runner-group grok-integrations";
            User = "root";
            Group = "root";
            Restart = "on-failure";
            RestartSec = "2s";
            TimeoutStopSec = "15s";
            UMask = "0077";

            NoNewPrivileges = true;
            # CAP_SYS_ADMIN is limited to the read-only 9P mount operation.
            # CAP_CHOWN is required to hand the root-created socket and mount
            # points to the unprivileged runner group.
            CapabilityBoundingSet = [
              "CAP_CHOWN"
              "CAP_SYS_ADMIN"
            ];
            AmbientCapabilities = [
              "CAP_CHOWN"
              "CAP_SYS_ADMIN"
            ];
            LockPersonality = true;
            MemoryDenyWriteExecute = true;
            RestrictAddressFamilies = [
              "AF_UNIX"
              "AF_VSOCK"
            ];
            RestrictNamespaces = true;
            RestrictRealtime = true;
            RestrictSUIDSGID = true;
            SystemCallArchitectures = "native";
            IPAddressDeny = "any";
          };
        };

        systemd.services.grok-integration-runner = {
          description = "Grok Desktop managed integration runner";
          wantedBy = [ "multi-user.target" ];
          requires = [ "grok-workspace-mounter.service" ];
          after = [
            "systemd-tmpfiles-setup.service"
            "grok-workspace-mounter.service"
          ];
          serviceConfig = {
            Type = "notify";
            ExecStart = "${lib.getExe' cfg.integrationRunner.package "grok-integration-runner"} --policy /etc/grok-desktop/policy.json";
            User = "grok-integrations";
            Group = "grok-integrations";
            WorkingDirectory = cfg.stateRoot;
            Restart = "on-failure";
            RestartSec = "2s";
            TimeoutStopSec = "15s";
            UMask = "0077";

            NoNewPrivileges = true;
            CapabilityBoundingSet = "";
            AmbientCapabilities = "";
            LockPersonality = true;
            PrivateDevices = true;
            PrivateTmp = true;
            ProtectClock = true;
            ProtectControlGroups = true;
            ProtectHome = true;
            ProtectHostname = true;
            ProtectKernelLogs = true;
            ProtectKernelModules = true;
            ProtectKernelTunables = true;
            ProtectSystem = "strict";
            RestrictAddressFamilies = [
              "AF_UNIX"
              "AF_VSOCK"
            ];
            RestrictRealtime = true;
            RestrictSUIDSGID = true;
            SystemCallArchitectures = "native";
            IPAddressDeny = "any";

            ReadOnlyPaths = [
              cfg.workspaceRoot
              "/etc/grok-desktop"
            ];
            ReadWritePaths = [
              cfg.stateRoot
              "/run/grok-desktop"
            ];
          };
        };
      })
    ]
  );
}
