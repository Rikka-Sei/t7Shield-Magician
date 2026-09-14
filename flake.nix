{
  description = "t7Shield Magician —— Samsung T7 Shield 磁盘解锁客户端（Rust + GTK4 + libadwaita）开发环境与应用打包（nix run . 启动 magi）";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem
      (system:
      let
        # D27：应用产物只面向 Linux 系统（用 system 字符串判定，不在输出结构层强制
        # 其它平台的 stdenv——nixpkgs 26.11 起 x86_64-darwin 已移除，强制求值会直接抛错）。
        isLinuxTarget = builtins.elem system [ "x86_64-linux" "aarch64-linux" ];
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

        # 图标主题：两平台 devShell 一致提供。缺失时 GtkIconTheme 只能命中 GTK4 内置
        # fallback 图标（view-grid/view-list 等），help-about-symbolic 等主题图标解析
        # 失败，导航行显示破损图片占位符。
        gtkIconThemeDeps = with pkgs; [
          adwaita-icon-theme
          hicolor-icon-theme
        ];

        # 仅 Linux 需要：GIO 扩展模块。
        gtkRuntimeDeps = gtkIconThemeDeps
          ++ lib.optionals stdenv.hostPlatform.isLinux (with pkgs; [
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

        # XDG_DATA_DIRS 两平台一致导出：GTK 的图标检索走该变量。此前仅 Linux 导出，
        # darwin devShell 里 adwaita-icon-theme 虽在 buildInputs 也不进搜索路径，
        # help-about-symbolic 等主题图标全部解析失败（Phase 1 探针 has_icon=NO 实证）。
        gtkIconThemeHook = ''
          export XDG_DATA_DIRS=${lib.makeSearchPath "share" [
            pkgs.gsettings-desktop-schemas
            pkgs.gtk4
            pkgs.adwaita-icon-theme
            pkgs.hicolor-icon-theme
          ]}''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}
        '';

        # 仅 Linux 需要导出的其余 GTK 运行时变量（GIO 扩展模块、GDK 后端）。
        # macOS 下 GIO 模块与显示后端走系统约定，覆盖反而破坏系统行为。
        gtkRuntimeHook = lib.optionalString stdenv.hostPlatform.isLinux ''
          export GIO_EXTRA_MODULES=${pkgs.glib-networking}/lib/gio/modules''${GIO_EXTRA_MODULES:+:$GIO_EXTRA_MODULES}

          export GDK_BACKEND=''${GDK_BACKEND:-wayland,x11}
        '';

        # ---- 应用打包：magi（仅 Linux；`nix build .#magi` / `nix run .`）----
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
          # 靠 XDG_DATA_DIRS 搜不到），并入 hook 的包装参数一次生效，
          # 免于 GLib-GIO-CRITICAL: g_settings_schema_source_lookup。
          preFixup = ''
            gappsWrapperArgs+=(--prefix GSETTINGS_SCHEMA_DIR : ${lib.concatStringsSep ":" gtkSchemaDirs})
          '';
          meta = with lib; {
            description = "MagiShield：Samsung T7 Shield 磁盘解锁客户端（GTK4 + libadwaita）";
            mainProgram = "magi";
            license = licenses.gpl3Only;
            platforms = platforms.linux; # D27：运行目标平台为仅 Linux
          };
        };

        # 开发环境不受运行平台限制：Darwin 开发机仍可用 devShell（spec/代码/审计都在此跑）。
        devShell = pkgs.mkShell {
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
            ${gtkIconThemeHook}
          '';
        };

        # D27：应用产物只面向 Linux（darwin 的 packages/apps 不再导出）。
        # 用纯 if 分支（isLinuxTarget 只依赖 system 字符串）：输出结构层不触碰
        # 其它平台的 pkgs/lib——nixpkgs 26.11 起 x86_64-darwin 已移除，任何 import 直接抛错。
        linuxOutputs = {
          packages.magi = magi;
          packages.default = magi;

          apps.default = {
            type = "app";
            program = "${magi}/bin/magi";
          };
        };
        darwinOutputs = { };
      in
      # if 与 // 都只在属性集结构层工作，不强制任何 pkgs 值。
      (if isLinuxTarget then linuxOutputs else darwinOutputs)
      // {
        devShells.default = devShell;
      });
}
