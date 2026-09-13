//! t7-transport：T7 Shield 传输层，`Transport` trait 的唯一权威定义处。
//!
//! 依赖方向：`t7-protocol` → `t7-transport`（反向禁止）。本 crate 只提供 12 字节 CDB
//! 的收发通道与设备描述符侦察，**不构造令牌流**——令牌流归 `t7-protocol`。
//!
//! 平台口径（spec 项目级约束）：Linux 为全功能目标平台，命令通道走 `SG_IO`；macOS 只做
//! 只读描述符侦察，任何盘操作立即返回 `TransportError::Unavailable`，不重试、不退避、
//! 不轮询。Linux 专属代码用 `cfg(target_os = "linux")` 门控，其可跨平台的纯逻辑
//! （CDB 构造、sense/errno 映射、描述符解析、重枚举判据）下沉到本 crate 的跨平台模块。
//!
//! W01 仅建立模块骨架：各模块目前只有职责说明，行为由后续任务填充。

/// 唯一的 `Transport` trait：`ScsiCdb` / `Direction` / `DeviceTarget` / `TransportError`。
pub mod transport;

/// §4.3 CDB 逐字节构造：`cdb_security_in` / `cdb_security_out`。
pub mod cdb;

/// `SenseData` 与 sense/SCSI 状态 → `TransportError` 映射（跨平台纯函数）。
pub mod sense;

/// OS 错误码 → `TransportError` 映射（跨平台纯函数）。
pub mod errno_map;

/// USB 配置描述符解析：`UsbDescriptorSummary` / `AlternateSetting` / `Endpoint`
/// 与 `parse_config_descriptor`（跨平台纯函数 + 真实描述符 fixture）。
pub mod usb_descriptor;

/// 重枚举轮询判据（纯逻辑）+ 观察采样结构。
pub mod reenumeration;

/// macOS 只读描述符侦察：`MacOsDiscovery`（`cfg(target_os = "macos")`）。
#[cfg(target_os = "macos")]
pub mod macos;

/// Linux 命令通道：`LinuxSgIo`（`cfg(target_os = "linux")`）。
#[cfg(target_os = "linux")]
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
