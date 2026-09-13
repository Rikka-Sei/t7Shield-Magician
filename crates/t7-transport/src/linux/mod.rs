//! 职责：Linux 命令通道入口——`LinuxSgIo`（`cfg(target_os = "linux")`）。

/// 职责：`SG_IO` ioctl 薄层：自建 `#[repr(C)]` 结构，本文件只做 syscall 包装。
pub mod sg_io;

/// 职责：sysfs 设备扫描与重枚举观察（`/sys/class/scsi_generic`、`/proc/partitions`、
/// `/proc/mounts`），全部为世界可读文件，不提权。
pub mod scan;
