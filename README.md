# t7Shield Magician

Samsung T7 Shield 磁盘解锁客户端（Rust + GTK4 + libadwaita）。协议逆向与格式说明见同级仓库 `../t7Shield-protocol`。

## 快速开始

```bash
# 进入开发环境（含 Rust 工具链、gtk4、libadwaita、pkg-config、gobject-introspection 等）
nix develop

# 使用 direnv 时自动加载（.envrc 已配置 use flake）
direnv allow

# 构建（当前仓库尚无源码，此命令为占位）
cargo build
```

环境自检：

```bash
pkg-config --modversion gtk4 libadwaita-1
cargo --version && rustc --version
```

## 平台支持

- **Linux**：全功能。
- **macOS**：可运行，但受系统限制无法直接发送 SCSI 命令，详见后续 spec。

## 协议与安全纪律

设备口令只在内存中使用，不落盘、不写入日志。
