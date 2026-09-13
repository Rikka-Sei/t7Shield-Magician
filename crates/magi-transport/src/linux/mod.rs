//! Linux 命令通道：`LinuxSgIo`（`SG_IO`）与 sysfs 设备扫描/重枚举观察。
//!
//! 可测性口径（K4）：本模块只把「真正调用 `ioctl`」与「打开设备节点」这两处平台动作做
//! `cfg` 分支（见 [`sg_io::submit`] 与 [`open_device_node`]），其余判定逻辑（SG_IO 头组装、
//! 完成判定、sysfs 扫描与重枚举采样）都是跨平台纯逻辑，非 Linux 开发机上也能跑测试。

pub mod scan;
pub mod sg_io;

use std::cell::{Cell, RefCell};
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use std::fs::OpenOptions;

use crate::errno_map::map_ioctl_errno;
#[cfg(target_os = "linux")]
use crate::errno_map::map_open_errno;
use crate::sense::{SenseData, SENSE_BUFFER_LEN};
use crate::transport::{DeviceTarget, Direction, ScsiCdb, Transport, TransportError};

use sg_io::{build_sg_io_hdr, check_completion, sense_from_buffer, submit};

/// Linux 传输实现：打开 `/dev/sgN` 并用 `SG_IO` 下发 12 字节 CDB（D07）。
///
/// 字段用 `Cell`/`RefCell` 承载「上次 sense / 上次状态」：协议层单飞约束下每个设备只有一个
/// 工作线程在飞（§6），因此无需额外锁。
pub struct LinuxSgIo {
    file: File,
    last_sense: RefCell<Option<SenseData>>,
    last_status: Cell<Option<u8>>,
}

impl LinuxSgIo {
    /// 最近一次命令的 sense（无记录时为 `None`，例如 GOOD 且 sense 缓冲全零）。
    pub fn last_sense(&self) -> Option<SenseData> {
        *self.last_sense.borrow()
    }

    /// 最近一次命令的 SCSI 状态字节。
    pub fn last_scsi_status(&self) -> Option<u8> {
        self.last_status.get()
    }
}

impl Transport for LinuxSgIo {
    fn open(target: &DeviceTarget) -> Result<Self, TransportError> {
        let DeviceTarget::LinuxSg(path) = target;
        Ok(Self {
            file: open_device_node(path)?,
            last_sense: RefCell::new(None),
            last_status: Cell::new(None),
        })
    }

    /// 一次 `SG_IO`：组装头 → ioctl → 回填 sense/状态 → 判定。
    ///
    /// 失败不重试（D11）；`ioctl` 失败按 errno 分类，其中超时语境携带实际耗时。
    fn execute(
        &self,
        cdb: &ScsiCdb,
        dir: Direction,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, TransportError> {
        let mut sense_buffer = [0u8; SENSE_BUFFER_LEN];
        let mut hdr = build_sg_io_hdr(cdb, dir, data, timeout);
        hdr.attach_sense(&mut sense_buffer);

        let started = Instant::now();
        let submitted = submit(self.file.as_raw_fd(), &mut hdr);
        let elapsed = started.elapsed();
        if let Err(error) = submitted {
            return Err(map_ioctl_errno(error.raw_os_error().unwrap_or(0), elapsed));
        }

        // 不论成败都回填：诊断需要「最后一条命令的 sense 与状态」（§4.4）。
        let sense = sense_from_buffer(&sense_buffer);
        self.last_sense.replace(sense);
        self.last_status.set(Some(hdr.status));
        check_completion(&hdr, sense, elapsed)
    }
}

/// 读写方式打开设备节点：命令通道需要双向（OUT 发报文、IN 收应答），只读打开无法下发 OUT。
#[cfg(target_os = "linux")]
fn open_device_node(path: &str) -> Result<File, TransportError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| map_open_errno(error.raw_os_error().unwrap_or(0)))
}

/// 非 Linux 平台没有 `SG_IO` 通道：`open` 直接返回通道不可用（D27）。
#[cfg(not(target_os = "linux"))]
fn open_device_node(_path: &str) -> Result<File, TransportError> {
    Err(TransportError::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 非 Linux 平台：设备节点路径一律返回通道不可用（不在本平台伪造 SG_IO）。
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn test_linux_sg_io_unavailable_off_linux() {
        let target = DeviceTarget::LinuxSg("/dev/sg0".to_string());
        assert_eq!(
            LinuxSgIo::open(&target).err(),
            Some(TransportError::Unavailable)
        );
    }
}
