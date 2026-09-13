{
  description = "t7Shield protocol reverse engineering toolbox";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        pythonEnv = pkgs.python312.withPackages (ps: with ps; [
            pyobjc-core
            pyobjc-framework-Cocoa
          construct
        ]);
      in
      {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            wireshark-cli   # tshark / rawshark
            pythonEnv       # pyobjc -> IOKit SCSI user client
            nodejs
            ripgrep
            jq
            binutils        # objdump / nm / strings
            file
            radare2         # 反汇编分析
          ];
          shellHook = ''
            echo "[t7Shield] RE toolbox ready"
          '';
        };
      });
}
