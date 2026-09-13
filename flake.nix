{
  description = "t7Shield Magician —— Samsung T7 Shield 磁盘解锁客户端（Rust + GTK4 + libadwaita）开发环境";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        inherit (pkgs) lib stdenv;

        # Rust 工具链：直接使用 nixpkgs 的稳定打包，避免 rustup 在 Nix 环境下的
        # 动态链接与 ~/.rustup 状态问题。
        rustToolchain = with pkgs; [
          cargo
          rustc
          rustfmt
          clippy
          rust-analyzer
        ];

        # GTK4 / libadwaita 开发依赖（Linux 与 Darwin 均可构建）
        gtkDevDeps = with pkgs; [
          gtk4
          libadwaita
          gobject-introspection
        ];

        # 仅 Linux 需要：dconf schema、图标主题、GIO 扩展模块。
        gtkRuntimeDeps = lib.optionals stdenv.hostPlatform.isLinux (with pkgs; [
          gsettings-desktop-schemas
          adwaita-icon-theme
          hicolor-icon-theme
          glib-networking
        ]);

        # 仅 Linux 需要导出的 GTK 运行时变量（dconf schema 搜索路径、GIO 扩展模块、
        # GDK 后端）。macOS 下 GTK/libadwaita 同样来自 nixpkgs，但 XDG 目录与 GIO
        # 模块走系统约定，覆盖这些变量反而会破坏系统行为，因此只在该平台导出。
        gtkRuntimeHook = lib.optionalString stdenv.hostPlatform.isLinux ''
          export XDG_DATA_DIRS=${lib.makeSearchPath "share" [
            pkgs.gsettings-desktop-schemas
            pkgs.gtk4
            pkgs.adwaita-icon-theme
            pkgs.hicolor-icon-theme
          ]}''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}

          export GIO_EXTRA_MODULES=${pkgs.glib-networking}/lib/gio/modules''${GIO_EXTRA_MODULES:+:$GIO_EXTRA_MODULES}

          export GDK_BACKEND=''${GDK_BACKEND:-wayland,x11}
        '';
      in
      {
        devShells.default = pkgs.mkShell {
          name = "t7shield-magician";

          nativeBuildInputs = with pkgs; [
            pkg-config
            # GTK4/libadwaita 应用打包期 setup hook（stdenv 构建/打包阶段自动处理
            # gschema、图标与运行时环境包装；交互式 devShell 中仅作为构建依赖提供）。
            # nixpkgs 已弃用旧名 wrapGAppsHook，GTK4 用 wrapGAppsHook4。
            wrapGAppsHook4
          ] ++ rustToolchain;

          buildInputs = gtkDevDeps ++ gtkRuntimeDeps ++ (with pkgs; [
            just     # 可选任务命令工具
            ripgrep  # 代码检索
          ]);

          # rust-analyzer 需要标准库源码才能给出 std 的跳转/补全。
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";

          shellHook = ''
            echo "[t7Shield Magician] Rust + GTK4 + libadwaita 开发环境就绪"
            echo "  rustc: $(rustc --version)"
            echo "  gtk4:  $(pkg-config --modversion gtk4 2>/dev/null || echo '未找到')"
            echo "  adw:   $(pkg-config --modversion libadwaita-1 2>/dev/null || echo '未找到')"
            ${gtkRuntimeHook}
          '';
        };
      });
}
