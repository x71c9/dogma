{ lib, config, environment, ... }:

# dogma-secrets — sops secret wiring driven by dogma.yml.
#
# Each host sets its machine role once in default.nix:
#
#   dogma.machine = "server_backend";
#
# Secrets are env-specific (each env's host has its own age key) but the config
# is shared across all envs: the env comes from the `environment` module arg
# (injected by flake.nix mkSystem), the machine role from dogma.machine. Secrets
# are auto-declared from the generated secrets/<env>/<machine>/secrets.nix
# (written by dogma deploy). No manual listing needed.
#
# To override permissions on a specific secret, add in default.nix:
#
#   dogma.secrets."database/password".mode = "0444";
#
# Secret name format is "group/field" matching dogma.yml exactly.
# Each secret is decrypted to /run/secrets/<group>/<field> at boot.

let
  secretsBase = ../../secrets;

  secretModule = lib.types.submodule ({ ... }: {
    options = {
      owner = lib.mkOption {
        type = lib.types.str;
        default = "root";
      };
      mode = lib.mkOption {
        type = lib.types.str;
        default = "0400";
      };
    };
  });

  machine = config.dogma.machine;

  # Auto-import generated secrets list if present. Falls back to empty if
  # dogma deploy has not been run yet (e.g. first checkout).
  secretsNixFile = "${secretsBase}/${environment}/${machine}/secrets.nix";
  generatedSecrets =
    if builtins.pathExists secretsNixFile
    then import secretsNixFile
    else { };

  # All secret names: union of generated + any explicit overrides.
  allNames = lib.attrNames (generatedSecrets // config.dogma.secrets);

in
{
  options.dogma = {
    machine = lib.mkOption {
      type = lib.types.str;
      description = "Dogma machine role (e.g. server_backend), env-agnostic. Set once in the host's default.nix; the env is injected separately by flake.nix.";
    };

    secrets = lib.mkOption {
      type = lib.types.attrsOf secretModule;
      default = { };
      description = ''
        Per-secret permission overrides. Keys are "group/field" matching dogma.yml.
        All secrets are auto-declared from secrets/<env>/<machine>/secrets.nix; only
        set this when you need non-default owner or mode on a specific secret.
      '';
    };

    secretPath = lib.mkOption {
      type = lib.types.functionTo lib.types.str;
      default = name: config.sops.secrets.${name}.path;
      description = "Returns the /run/secrets runtime path for a secret by name (group/field).";
    };

    secretNames = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = allNames;
      description = "All secret names declared for this machine.";
    };
  };

  config = {
    sops.validateSopsFiles = false;
    sops.age.sshKeyPaths = [ "/etc/ssh/ssh_host_ed25519_key" ];
    sops.age.generateKey = false;

    sops.secrets = lib.listToAttrs (map (name:
      let
        parts = lib.splitString "/" name;
        group = lib.head parts;
        field = lib.concatStringsSep "/" (lib.tail parts);
        override = config.dogma.secrets.${name} or {};
        owner = override.owner or "root";
        mode  = override.mode  or "0400";
      in
      lib.nameValuePair name {
        sopsFile = "${secretsBase}/${environment}/${machine}/${group}.yaml";
        key      = field;
        inherit owner mode;
      }
    ) allNames);
  };
}
