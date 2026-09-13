# t7Shield Magician

Samsung T7 Shield 磁盘解锁客户端（Rust + GTK4 + libadwaita），可执行文件名为 `magi`。协议逆向与格式说明见同级仓库 `../t7Shield-protocol`；界面布局对标原版 Samsung Magician（深色侧边栏、设备卡片与状态徽章、诊断页）。

## 快速开始

```bash
# 进入开发环境（rustc/cargo/clippy/rustfmt/rust-analyzer、python3、gtk4、libadwaita、pkg-config）
nix develop

# 使用 direnv 时自动加载（.envrc 已配置 use flake）
direnv allow

# 构建并运行：产物为 target/debug/magi
cargo run
cargo build
```

## crate 结构

| crate | 职责 |
|---|---|
| `magi-protocol` | 纯协议层：Level-0 Discovery 解析、TCG 帧构造与响应解析、会话状态机与解锁判据 |
| `magi-transport` | 传输层：`Transport` trait、12 字节 CDB 通道、平台实现（Linux `sg_io`／macOS 只读描述符侦察） |
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

- **Linux**：全功能。
- **macOS**：可运行，但受系统限制无法直接发送 SCSI 命令，详见 spec。
- 交付后的 Linux 真机验证清单见 `plans/2026-09-14-t7-magician-implementation.md` 的 V01 节。

## 协议与安全纪律

设备口令只在内存中使用，构造完成后立即清零，不落盘、不入日志。
