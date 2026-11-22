{
  description = "dogma NixOS modules and dev shell";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs = { self, nixpkgs, ... }: {
    nixosModules = {
      dogma-secrets   = import ./nix-modules/dogma-secrets.nix;
      dogma-container = import ./nix-modules/dogma-container.nix;
    };

    devShells = nixpkgs.lib.genAttrs [
      "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin"
    ] (system: {
      default = import ./shell.nix { pkgs = nixpkgs.legacyPackages.${system}; };
    });
  };
}
