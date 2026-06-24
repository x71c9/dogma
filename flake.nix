{
  description = "dogma NixOS modules";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";

  outputs = { self, nixpkgs, ... }: {
    nixosModules = {
      dogma-secrets   = import ./nix-modules/dogma-secrets.nix;
      dogma-container = import ./nix-modules/dogma-container.nix;
    };
  };
}
