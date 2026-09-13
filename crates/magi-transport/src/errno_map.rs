//! OS / SG_IO 层错误码 → [`TransportError`] 映射（跨平台纯函数，无需任何 syscall）。
//!
//! 映射口径取自 §5 的「产生条件（事实）」列：`EACCES`/`EPERM` → [`TransportError::PermissionDenied`]，
//! `ENODEV`/`ENXIO` → [`TransportError::DeviceGone`]，超时语境 → [`TransportError::Timeout`]
//! （并原样携带实际耗时），其余一律 [`TransportError::Platform`] 兜底并保留原始码。

use std::time::Duration;

use crate::cdb::CMD_TIMEOUT;
use crate::transport::TransportError;

/// SG_IO `host_status`：命令正常完成（`DID_OK`，Linux `include/scsi/scsi.h`）。
pub const SG_HOST_DID_OK: u16 = 0x00;

/// SG_IO `host_status`：命令超时（`DID_TIME_OUT`）。
pub const SG_HOST_DID_TIME_OUT: u16 = 0x03;

/// `ioctl` 返回 `-1` 时的 errno → [`TransportError`]。
///
/// `EIO` 只在「已耗满命令超时窗口」时才判为超时（USB 传输层偶尔把设备超时上报为 `EIO`）；
/// 未耗满超时窗口的 `EIO` 是普通 I/O 失败，走 `Platform` 兜底。
pub fn map_ioctl_errno(raw: i32, elapsed: Duration) -> TransportError {
    match raw {
        libc::EACCES | libc::EPERM => TransportError::PermissionDenied,
        libc::ENODEV | libc::ENXIO => TransportError::DeviceGone,
        libc::ETIMEDOUT => TransportError::Timeout { elapsed },
        libc::EIO if elapsed >= CMD_TIMEOUT => TransportError::Timeout { elapsed },
        other => TransportError::Platform {
            code: i64::from(other),
        },
    }
}

/// `open(2)` 失败时的 errno → [`TransportError`]。
///
/// `ENOENT` 与 `ENODEV`/`ENXIO` 同为「设备节点不存在」，即重枚举期间节点消失（§5 的 `DeviceGone` 事实）。
pub fn map_open_errno(raw: i32) -> TransportError {
    match raw {
        libc::ENOENT | libc::ENODEV | libc::ENXIO => TransportError::DeviceGone,
        libc::EACCES | libc::EPERM => TransportError::PermissionDenied,
        other => TransportError::Platform {
            code: i64::from(other),
        },
    }
}

/// `sg_io_hdr.host_status` → [`TransportError`]；`None` 表示主机侧无错误（可继续看 SCSI 状态）。
///
/// 只有 `DID_TIME_OUT` 具有 §5 明确定义的分类（超时且携带实际耗时）；其余主机字节（`DID_NO_CONNECT`
/// 等 SCSI 层事实）按 §5「既非 GOOD 也非 CHECK CONDITION 的完成状态」走 `Platform` 兜底并保留原始值。
pub fn map_sg_io_host_status(host_status: u16, elapsed: Duration) -> Option<TransportError> {
    match host_status {
        SG_HOST_DID_OK => None,
        SG_HOST_DID_TIME_OUT => Some(TransportError::Timeout { elapsed }),
        other => Some(TransportError::Platform {
            code: i64::from(other),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 锚点：`test_command_timeout_maps_to_timeout_error`（spec §10、§7「命令超时」）。
    #[test]
    fn test_command_timeout_maps_to_timeout_error() {
        let elapsed = Duration::from_secs(30);
        let mapped = map_ioctl_errno(libc::ETIMEDOUT, elapsed);
        assert_eq!(mapped, TransportError::Timeout { elapsed });
        assert_eq!(
            mapped,
            TransportError::Timeout {
                elapsed: Duration::from_secs(30)
            },
            "elapsed 必须原样携带，不是丢弃的占位"
        );

        assert_eq!(
            map_ioctl_errno(libc::ENODEV, elapsed),
            TransportError::DeviceGone
        );
        assert_eq!(
            map_ioctl_errno(libc::ENXIO, elapsed),
            TransportError::DeviceGone
        );
        assert_eq!(
            map_ioctl_errno(libc::EACCES, elapsed),
            TransportError::PermissionDenied
        );
        assert_eq!(
            map_ioctl_errno(libc::EPERM, elapsed),
            TransportError::PermissionDenied
        );
        assert_eq!(
            map_ioctl_errno(libc::EINVAL, elapsed),
            TransportError::Platform {
                code: i64::from(libc::EINVAL)
            }
        );

        assert_eq!(CMD_TIMEOUT, Duration::from_secs(30));
    }

    /// `EIO` 只在耗满超时窗口时算超时；提前失败的 `EIO` 是普通 I/O 失败。
    #[test]
    fn test_eio_timeout_only_after_full_window() {
        assert_eq!(
            map_ioctl_errno(libc::EIO, Duration::from_millis(12)),
            TransportError::Platform {
                code: i64::from(libc::EIO)
            }
        );
        let elapsed = Duration::from_secs(30);
        assert_eq!(
            map_ioctl_errno(libc::EIO, elapsed),
            TransportError::Timeout { elapsed }
        );
    }

    /// 节点不存在（`ENOENT`）同样按 `DeviceGone` 处置：重枚举期间 `/dev/sgN` 会消失。
    #[test]
    fn test_open_errno_mapping() {
        assert_eq!(map_open_errno(libc::ENOENT), TransportError::DeviceGone);
        assert_eq!(
            map_open_errno(libc::EACCES),
            TransportError::PermissionDenied
        );
        assert_eq!(
            map_open_errno(libc::EISDIR),
            TransportError::Platform {
                code: i64::from(libc::EISDIR)
            }
        );
    }

    #[test]
    fn test_sg_io_host_status_mapping() {
        let elapsed = Duration::from_secs(30);
        assert_eq!(map_sg_io_host_status(SG_HOST_DID_OK, elapsed), None);
        assert_eq!(
            map_sg_io_host_status(SG_HOST_DID_TIME_OUT, elapsed),
            Some(TransportError::Timeout { elapsed })
        );
        assert_eq!(
            map_sg_io_host_status(0x01, elapsed),
            Some(TransportError::Platform { code: 0x01 })
        );
    }
}
