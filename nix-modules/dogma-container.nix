{ config, lib, pkgs, ... }:

# Reusable helper for declaring NixOS containers with four features wired
# automatically:
#
#   1. Secrets bind-mounts — secrets listed in `secrets` are bind-mounted from
#      the host into the container at their sops runtime paths. Set `allSecrets
#      = true` to mount every host secret automatically.
#
#   2. Journal linking — sets `LinkJournal=host` via a systemd-nspawn drop-in.
#
#   3. `secrets-env` wrapper — injected into every container's PATH. Reads all
#      files under /run/secrets/, derives env var names from paths:
#        /run/secrets/backend/api_domain  →  BACKEND_API_DOMAIN
#        /run/secrets/stripe/secret_key   →  STRIPE_SECRET_KEY
#      Then exec-s its arguments with those vars set. Use as ExecStart:
#        ExecStart = "${secretsEnv}/bin/secrets-env ${pkgs.nodejs}/bin/node server.js";
#
#   4. Internet access (opt-in) — set `allowInternetAccess = true` together with
#      `externalInterface` to enable outbound NAT for the container. Also requires
#      the container config to enable systemd-resolved (NixOS issue #162686):
#        networking.useHostResolvConf = lib.mkForce false;
#        services.resolved.enable = true;
#      Disabled by default — only enable when the container genuinely needs to
#      reach external hosts (e.g. third-party APIs).
#
# Usage:
#
#   dogmaContainers.mmrb-be = {
#     hostAddress  = "10.10.1.1";
#     localAddress = "10.10.1.3";
#     allSecrets   = true;
#     allowInternetAccess = true; externalInterface = "enp1s0";
#     config = { ... }: { ... };
#   };

let
  secretsEnvPkg = pkgs.writeShellApplication {
    name = "secrets-env";
    text = ''
      if [ -d /run/secrets ]; then
        while IFS= read -r -d "" secret_file; do
          var_name="$(
            printf '%s' "''${secret_file#/run/secrets/}" \
              | tr '/' '_' | tr '-' '_' | tr '[:lower:]' '[:upper:]'
          )"
          value="$(cat "$secret_file")"
          export "$var_name=$value"
        done < <(find /run/secrets -type f -print0)
      fi
      exec "$@"
    '';
  };

  containerOpts = { ... }: {
    options = {
      hostAddress = lib.mkOption {
        type = lib.types.str;
        description = "IP of the host side of the veth pair (e.g. 10.10.1.1).";
      };

      localAddress = lib.mkOption {
        type = lib.types.str;
        description = "IP of the container side of the veth pair (e.g. 10.10.1.3).";
      };

      secrets = lib.mkOption {
        type    = lib.types.listOf lib.types.str;
        default = [];
        description = ''
          Secret names (group/field) to bind-mount into the container. Must be
          a subset of the host's declared dogma secrets. Leave empty for none.
        '';
      };

      allSecrets = lib.mkOption {
        type    = lib.types.bool;
        default = false;
        description = ''
          When true, bind-mount ALL host dogma secrets into the container.
          Ignored when `secrets` is non-empty.
        '';
      };

      allowInternetAccess = lib.mkOption {
        type    = lib.types.bool;
        default = false;
        description = ''
          When true, enable outbound NAT on the host so the container can reach
          the internet. Requires `externalInterface` to be set.
          The container config must also enable systemd-resolved:
            networking.useHostResolvConf = lib.mkForce false;
            services.resolved.enable = true;
        '';
      };

      externalInterface = lib.mkOption {
        type    = lib.types.str;
        default = "";
        description = ''
          Host network interface used as the NAT gateway (e.g. "enp1s0").
          Required when `allowInternetAccess = true`.
        '';
      };

      extraBindMounts = lib.mkOption {
        type    = lib.types.attrsOf (lib.types.submodule {
          options = {
            hostPath   = lib.mkOption { type = lib.types.str; };
            isReadOnly = lib.mkOption { type = lib.types.bool; default = true; };
          };
        });
        default     = {};
        description = ''
          Additional bind-mounts to pass into the container alongside the
          auto-generated secret mounts. Keys are the in-container paths.
        '';
      };

      config = lib.mkOption {
        description = "NixOS configuration module for the container.";
      };
    };
  };

  _secretsFor = cfg:
    if cfg.secrets != []   then cfg.secrets
    else if cfg.allSecrets then config.dogma.secretNames
    else [];

in
{
  options.dogmaContainers = lib.mkOption {
    type    = lib.types.attrsOf (lib.types.submodule containerOpts);
    default = {};
    description = "Declarative NixOS containers managed by the dogma template.";
  };

  options.dogma.secretsEnv = lib.mkOption {
    type        = lib.types.package;
    default     = secretsEnvPkg;
    description = "The secrets-env wrapper package, available for use in container ExecStart.";
  };

  config = lib.mkIf (config.dogmaContainers != {}) {

    # ── 1. Container declarations ────────────────────────────────────────────
    containers = lib.mapAttrs (name: cfg:
      let effectiveSecrets = _secretsFor cfg;
      in {
        autoStart      = true;
        privateNetwork = true;
        hostAddress    = cfg.hostAddress;
        localAddress   = cfg.localAddress;

        bindMounts =
          lib.listToAttrs (map (secretName: {
            name  = config.sops.secrets.${secretName}.path;
            value = {
              hostPath   = config.sops.secrets.${secretName}.path;
              isReadOnly = true;
            };
          }) effectiveSecrets)
          // cfg.extraBindMounts;

        config = cfg.config;
      }
    ) config.dogmaContainers;

    # Stop timeout so systemd escalates to SIGKILL and releases the machine
    # registration before the next start attempt.
    systemd.services = lib.mapAttrs' (name: _: {
      name  = "container@${name}";
      value.serviceConfig = {
        TimeoutStopSec = 30;
        SendSIGKILL    = true;
      };
    }) config.dogmaContainers;

    # ── 2. Journal linking ───────────────────────────────────────────────────
    environment.etc = lib.mapAttrs' (name: _: {
      name  = "systemd/nspawn/${name}.nspawn";
      value.text = ''
        [Exec]
        LinkJournal=host
      '';
    }) config.dogmaContainers;

    networking.firewall.trustedInterfaces = [ "ve-+" ];

    # ── 4. Outbound NAT (opt-in per container) ───────────────────────────────
    networking.nat = lib.mkMerge (lib.mapAttrsToList (name: cfg:
      lib.mkIf cfg.allowInternetAccess {
        enable             = true;
        internalInterfaces = [ "ve-${name}" ];
        externalInterface  = cfg.externalInterface;
      }
    ) config.dogmaContainers);
  };
}
