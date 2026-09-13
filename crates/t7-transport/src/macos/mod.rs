//! 职责：macOS 只读描述符侦察入口——`MacOsDiscovery`（`cfg(target_os = "macos")`）。
//! 只拿到描述符字节，不打开设备、不 claim、不 seize。

/// 职责：IOKit 注册表 + `IOUSBDeviceInterface` 插件 FFI（`GetConfigurationDescriptorPtr`）。
pub mod iokit;
