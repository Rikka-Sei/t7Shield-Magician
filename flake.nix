{
  description = "t7Shield Magician —— Samsung T7 Shield 磁盘解锁客户端（Rust + GTK4 + libadwaita）开发环境与应用打包（nix run . 启动 magi）";

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

        # GTK4/libadwaita 启动时需要 dconf schema（org.gnome.desktop.* 等）。缺失时 GIO 会
        # 打印 g_settings_schema_source_lookup 的 GLib-GIO-CRITICAL 噪声，两平台都要提供。
        gtkSchemaDeps = with pkgs; [
          gsettings-desktop-schemas
        ];

        # 仅 Linux 需要：图标主题、GIO 扩展模块。
        gtkRuntimeDeps = lib.optionals stdenv.hostPlatform.isLinux (with pkgs; [
          adwaita-icon-theme
          hicolor-icon-theme
          glib-networking
        ]);

        # 开发期工具：spec 审计/屏障脚本、任务运行器、代码检索。
        # python3 必须来自 nixpkgs：darwin 上 stdenv 会导出 DEVELOPER_DIR/SDKROOT（apple-sdk），
        # 导致 /usr/bin/python3 这个 shim 报 `error: tool 'python3' not found`。
        devTools = with pkgs; [
          python3
          just
          ripgrep
        ];

        # GSettings 搜索路径：nixpkgs 把 schema 装在
        # share/gsettings-schemas/<pname>-<version>/glib-2.0/schemas，而不是
        # share/glib-2.0/schemas，因此 XDG_DATA_DIRS 永远搜不到，必须用
        # GSETTINGS_SCHEMA_DIR 显式指路。否则 GTK/libadwaita 启动时
        # g_settings_schema_source_get_default() 返回 NULL，stderr 出现
        # GLib-GIO-CRITICAL: g_settings_schema_source_lookup: assertion 'source != NULL' failed。
        gtkSchemaDirs = [
          "${pkgs.gtk4}/share/gsettings-schemas/${pkgs.gtk4.name}/glib-2.0/schemas"
          "${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}/glib-2.0/schemas"
        ];

        # 两平台一致导出（darwin 也必须显式给出）。
        gtkSchemaHook = ''
          export GSETTINGS_SCHEMA_DIR=${lib.concatStringsSep ":" gtkSchemaDirs}''${GSETTINGS_SCHEMA_DIR:+:$GSETTINGS_SCHEMA_DIR}
        '';

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

        # ---- 应用打包：magi（`nix build .#magi` / `nix run .`）----
        # 版本取自 workspace.package.version，避免与 Cargo.toml 漂移。
        magi = pkgs.rustPlatform.buildRustPackage {
          pname = "magi";
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

          # flake 源 = git 已跟踪文件（远程 `nix run github:<owner>/t7Shield-Magician`
          # 只看已提交内容，改动 flake.nix/README/Cargo.lock 后必须先提交）。
          src = self;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [
            pkg-config
            # 打包期 hook：收集 gschema/图标/GIO 模块路径并包装 bin/magi
            # （gappsWrapperArgs）。nixpkgs 已弃用旧名 wrapGAppsHook，GTK4 用 wrapGAppsHook4。
            wrapGAppsHook4
          ];

          buildInputs = with pkgs; [
            gtk4
            libadwaita
          ] ++ gtkSchemaDeps ++ gtkRuntimeDeps;

          # GTK 模板实例化类测试需要图形会话，无头构建机必然跳过；
          # 测试验证走 devShell（cargo test --workspace），不在打包期跑。
          doCheck = false;

          # wrapGAppsHook4 未注入 GSETTINGS_SCHEMA_DIR（devShell 同款问题：nixpkgs 的
          # schema 布局是 share/gsettings-schemas/<pname>-<version>/glib-2.0/schemas，
          # 靠 XDG_DATA_DIRS 搜不到），并入 hook 的包装参数一次生效，两平台统一
          # 免于 GLib-GIO-CRITICAL: g_settings_schema_source_lookup。
          preFixup = ''
            gappsWrapperArgs+=(--prefix GSETTINGS_SCHEMA_DIR : ${lib.concatStringsSep ":" gtkSchemaDirs})
          '';

          meta = with lib; {
            description = "MagiShield：Samsung T7 Shield 磁盘解锁客户端（GTK4 + libadwaita）";
            mainProgram = "magi";
            license = licenses.gpl3Only;
            platforms = platforms.unix;
          };
        };

      in
      {
        packages.magi = magi;
        packages.default = magi;

        apps.default = {
          type = "app";
          program = "${magi}/bin/magi";
        };

        devShells.default = pkgs.mkShell {
          name = "t7shield-magician";

          nativeBuildInputs = with pkgs; [
            pkg-config
            # GTK4/libadwaita 应用打包期 setup hook（stdenv 构建/打包阶段自动处理
            # gschema、图标与运行时环境包装；交互式 devShell 中仅作为构建依赖提供）。
            # nixpkgs 已弃用旧名 wrapGAppsHook，GTK4 用 wrapGAppsHook4。
            wrapGAppsHook4
          ] ++ rustToolchain;

          buildInputs = gtkDevDeps ++ gtkSchemaDeps ++ gtkRuntimeDeps ++ devTools;

          # rust-analyzer 需要标准库源码才能给出 std 的跳转/补全。
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";

          shellHook = ''
            echo "[t7Shield Magician] Rust + GTK4 + libadwaita 开发环境就绪"
            echo "  rustc: $(rustc --version)"
            echo "  gtk4:  $(pkg-config --modversion gtk4 2>/dev/null || echo '未找到')"
            echo "  adw:   $(pkg-config --modversion libadwaita-1 2>/dev/null || echo '未找到')"
            ${gtkSchemaHook}
            ${gtkRuntimeHook}
          '';
        };
      });
}
