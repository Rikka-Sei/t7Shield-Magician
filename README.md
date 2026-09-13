# t7Shield Magician

Samsung T7 Shield 磁盘解锁客户端（Rust + GTK4 + libadwaita），可执行文件名为 `magi`。协议逆向与格式说明见同级仓库 `../t7Shield-protocol`；界面布局对标原版 Samsung Magician（深色侧边栏、设备卡片与状态徽章、诊断页）。

## 快速开始

```bash
# 进入开发环境（rustc/cargo/clippy/rustfmt/rust-analyzer、python3、gtk4、libadwaita、pkg-config）
nix develop

# 使用 direnv 时自动加载（.envrc 已配置 use flake）
direnv allow

# 一键构建并启动 MagiShield GUI（Nix 打包，release 版二进制 magi）
nix run .

# 构建并运行：产物为 target/debug/magi
cargo run
cargo build
```

## 远程启动

- **本机**：`nix run .`（等价 `nix run .#magi`）。
- **远程 Linux 机器（盘插在那台机上）**：Nix 从 git 仓库取 flake 时**只看已提交文件**——先把 flake.nix、README.md、Cargo.lock（及其他改动）提交并推送，再在该机本机的图形会话里运行 `nix run github:<owner>/t7Shield-Magician`，或把仓库放在该机可达路径后 `nix run /path/to/repo`。
- **无显示器场景**：`GDK_BACKEND=x11` + `ssh -Y` 走 X11 转发（Wayland 远程不在支持范围）。
- **设备权限**：解锁走 `sg_io`，需要对 `/dev/sg*` 的读写权限（udev 规则或加入相应组），交付后真机验证清单见 `plans/2026-09-14-t7-magician-implementation.md` 的 V01 节。

## crate 结构

| crate | 职责 |
|---|---|
| `magi-protocol` | 纯协议层：Level-0 Discovery 解析、TCG 帧构造与响应解析、会话状态机与解锁判据 |
| `magi-transport` | 传输层：`Transport` trait、12 字节 CDB 通道、Linux `sg_io` 实现与跨平台纯逻辑（sense/errno/描述符解析/重枚举采样） |
| `magi-app` | GTK4 + libadwaita 界面与线程编排（二进制名 `magi`） |

依赖方向单向：`magi-app` → `magi-protocol` → `magi-transport`。

## 验证

```bash
cargo test --workspace

# spec 三件套，均在 nix develop 内按顺序运行
python3 docs/specs/t7-magician/tools/test_audit_spec.py
python3 docs/specs/t7-magician/tools/audit_spec.py
python3 docs/specs/t7-magician/tools/barriers.py
```

## 平台支持

- **Linux（x86_64 / aarch64）：全功能**，也是唯一的运行目标平台（spec D27）。
- Nix 打包产物（`packages` / `apps`）只在 Linux 系统上提供；Darwin 仅保留 `devShells` 作为开发环境。
- 交付后的 Linux 真机验证清单见 `plans/2026-09-14-t7-magician-implementation.md` 的 V01 节。

## 协议与安全纪律

设备口令只在内存中使用，构造完成后立即清零，不落盘、不入日志。
