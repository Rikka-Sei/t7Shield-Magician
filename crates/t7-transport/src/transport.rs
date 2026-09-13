//! §4.3 传输层契约：`Transport` trait 与传输层类型（`ScsiCdb` / `Direction` /
//! `DeviceTarget` / `TransportError`）。本文件是本 crate 的唯一权威定义处，协议层只消费。
//!
//! 平台口径（§4.5、D08）：Linux 走 `SG_IO`（`crate::linux`）；macOS 只做只读描述符侦察
//! （`crate::macos`），任何盘操作立即返回 [`TransportError::Unavailable`]，不重试、不退避、不轮询。

use std::fmt;
use std::time::Duration;

use crate::sense::SenseData;

/// 12 字节 SCSI CDB（§4.3：只允许 SECURITY PROTOCOL IN/OUT 两类经本 trait 下发）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScsiCdb(pub [u8; 12]);

/// 数据相方向：`In` = 从设备收（SECURITY PROTOCOL IN），`Out` = 向设备发（SECURITY PROTOCOL OUT）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
}

/// 传输层目标：Linux 设备节点路径；macOS 只读侦察按 VID/PID 定位设备。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceTarget {
    LinuxSg(String),
    MacOsUsb { vid: u16, pid: u16 },
}

/// 传输层错误（§5）：SCSI 语义错误与平台错误码分列，平台码由 [`TransportError::Platform`] 兜底承载（D18）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// 平台通道不存在（macOS 盘操作、通道缺失）：不重试（D08）。
    Unavailable,
    /// `open`/`ioctl` 因权限失败（`EACCES`/`EPERM`）：不自身提权。
    PermissionDenied,
    /// 设备节点在重枚举期间消失（`ENODEV`/`ENXIO`）。
    DeviceGone,
    /// 单条命令超过 §6 的超时（30 s），携带实际耗时。
    Timeout { elapsed: Duration },
    /// 返回字节数不足以构成 `0x38` 字节报文头。
    ShortResponse { got: usize },
    /// SCSI 状态为 CHECK CONDITION，包装 sense。
    ScsiCheckCondition { sense: SenseData },
    /// 平台原始错误码（IOKit `kern_return_t`、非 SCSI 语义的完成状态等）；呈现码 `TransportFailure`。
    Platform { code: i64 },
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => write!(f, "平台通道不可用"),
            Self::PermissionDenied => write!(f, "设备节点权限不足"),
            Self::DeviceGone => write!(f, "设备节点已消失"),
            Self::Timeout { elapsed } => write!(f, "命令超时（已耗时 {:?}）", elapsed),
            Self::ShortResponse { got } => write!(f, "应答过短（仅 {} 字节）", got),
            Self::ScsiCheckCondition { sense } => write!(
                f,
                "SCSI CHECK CONDITION（key {:#04x} asc {:#04x} ascq {:#04x}）",
                sense.sense_key, sense.asc, sense.ascq
            ),
            Self::Platform { code } => write!(f, "平台错误码 {code}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// 唯一的 SCSI 命令收发通道（D07）：实现方各自负责平台 syscall，协议层只依赖本 trait。
pub trait Transport: Send {
    /// 打开目标通道；失败按 §5 分类（不重试、不提权）。
    fn open(target: &DeviceTarget) -> Result<Self, TransportError>
    where
        Self: Sized;

    /// 执行一条 SCSI 命令；`data` 为数据相缓冲（IN 为收，OUT 为发），返回实际传输字节数。
    fn execute(
        &self,
        cdb: &ScsiCdb,
        dir: Direction,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, TransportError>;
}
