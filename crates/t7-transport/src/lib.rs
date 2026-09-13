//! t7-transport：T7 Shield 传输层，`Transport` trait 的唯一权威定义处。
//!
//! 依赖方向：`t7-protocol` → `t7-transport`（反向禁止）。本 crate 只提供 12 字节 CDB
//! 的收发通道与设备描述符侦察，**不构造令牌流**——令牌流归 `t7-protocol`。
//!
//! 平台口径（spec 项目级约束）：Linux 为全功能目标平台，命令通道走 `SG_IO`；macOS 只做
//! 只读描述符侦察，任何盘操作立即返回 [`transport::TransportError::Unavailable`]，不重试、
//! 不退避、不轮询（D08）。
//!
//! 可测性口径（K4）：只有真正的平台 syscall 使用 `cfg(target_os = ...)` 门控——macOS 侧是
//! IOKit 注册表调用，Linux 侧是 `ioctl(SG_IO)`。其余全部为跨平台纯逻辑（CDB 构造、sense 与
//! errno 映射、描述符解析、重枚举采样、SG_IO 头组装），因此在 macOS 上也能完整跑测试。

/// 唯一的 `Transport` trait：`ScsiCdb` / `Direction` / `DeviceTarget` / `TransportError`。
pub mod transport;

/// §4.3 CDB 逐字节构造：`cdb_security_in` / `cdb_security_out` 与 §6 的固定长度/超时常量。
pub mod cdb;

/// `SenseData` 与 sense/SCSI 完成状态 → `TransportError` 映射（跨平台纯函数）。
pub mod sense;

/// OS 错误码与 SG_IO 主机状态 → `TransportError` 映射（跨平台纯函数）。
pub mod errno_map;

/// USB 配置描述符解析：`UsbDescriptorSummary` / `AlternateSetting` / `Endpoint`
/// 与 `parse_config_descriptor`（跨平台纯函数 + 真机 fixture）。
pub mod usb_descriptor;

/// 重枚举观察：轮询采样结构、窗口/间隔与 `poll_reenumeration`（纯逻辑 + 可注入探针）。
pub mod reenumeration;

/// macOS 只读描述符侦察：`MacOsDiscovery`（注册表只读，不打开设备、不 claim、不发 CDB）。
#[cfg(target_os = "macos")]
pub mod macos;

/// Linux 命令通道：`LinuxSgIo`（`SG_IO`）与 sysfs 设备扫描/重枚举观察。
pub mod linux;

#[cfg(test)]
mod tests {
    /// W01 冒烟测试：确认 crate 可编译且元数据可见（脚手架验收，非行为测试）。
    #[test]
    fn test_crate_builds() {
        assert_eq!(env!("CARGO_PKG_NAME"), "t7-transport");
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
    }
}
