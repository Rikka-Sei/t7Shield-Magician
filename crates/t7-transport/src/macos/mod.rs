//! macOS 只读描述符侦察入口：`MacOsDiscovery`（`cfg(target_os = "macos")`）。
//!
//! 只拿到描述符字节，不打开设备、不 claim、不 seize、不发任何 CDB。字节 → 结构的解析在跨平台的
//! [`crate::usb_descriptor`]（K2），因此真机 fixture 的解析测试在两平台跑的是同一份实现。

pub mod iokit;

use std::time::Duration;

use crate::transport::{DeviceTarget, Direction, ScsiCdb, Transport, TransportError};
use crate::usb_descriptor::{parse_config_descriptor, UsbDescriptorSummary};

/// macOS 通道：只读描述符侦察 + 盘操作恒不可用（D08 的已证实平台限制）。
pub struct MacOsDiscovery;

impl MacOsDiscovery {
    /// 只读描述符侦察：不 seize 设备、不打开接口、不发送 CDB。
    ///
    /// 返回注册表中 VID/PID 匹配的全部设备（同型号多台即多条），每条含该设备配置描述符解析出的
    /// 备用设置列表。
    pub fn enumerate(vid: u16, pid: u16) -> Result<Vec<UsbDescriptorSummary>, TransportError> {
        let raw = iokit::read_config_descriptors(vid, pid)?;
        let mut summaries = Vec::with_capacity(raw.len());
        for device in raw {
            summaries.push(parse_config_descriptor(
                device.vid,
                device.pid,
                &device.config_descriptor,
            )?);
        }
        Ok(summaries)
    }
}

impl Transport for MacOsDiscovery {
    fn open(_target: &DeviceTarget) -> Result<Self, TransportError> {
        Ok(MacOsDiscovery)
    }

    /// macOS 上没有可用的 SCSI 通道（`issues/2026-09-14-macOS传输通道.md`）：无条件返回
    /// [`TransportError::Unavailable`]，不发送任何命令、不重试、不退避、不轮询（D08）。
    fn execute(
        &self,
        _cdb: &ScsiCdb,
        _dir: Direction,
        _data: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, TransportError> {
        Err(TransportError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdb::{cdb_security_in, CMD_TIMEOUT, TCG_ALLOC_LEN};

    /// 锚点：`test_macos_transport_unavailable`（spec §10、§7「macOS 盘操作」）。
    ///
    /// 无条件运行（不依赖是否接入设备）：`open` 必须成功，任何 `execute` 必须立即返回
    /// `Unavailable`，且**一个字节都不写**数据相缓冲——证明「未发起」而不是「发起后失败」，
    /// 也证明没有重试或退避。
    #[test]
    fn test_macos_transport_unavailable() {
        let target = DeviceTarget::MacOsUsb {
            vid: 0x04E8,
            pid: 0x61FC,
        };
        let transport = MacOsDiscovery::open(&target).expect("macOS 上 open 必须成功");

        // ComID 用测试局部变量（D03：实测值不得写成生产常量）。
        let observed_comid: u16 = 0x1004;
        let mut buffer = [0xA5_u8; TCG_ALLOC_LEN as usize];

        for dir in [Direction::In, Direction::Out] {
            let result = transport.execute(
                &cdb_security_in(observed_comid, TCG_ALLOC_LEN),
                dir,
                &mut buffer,
                CMD_TIMEOUT,
            );
            assert_eq!(result, Err(TransportError::Unavailable));
        }

        assert!(
            buffer.iter().all(|byte| *byte == 0xA5),
            "通道不可用时不得写入数据相缓冲的任何一字节"
        );
    }

    /// 真机枚举：本机有锁定态 T7 Shield 时做真断言；无设备时提示后通过（保证无硬件机器上屏障确定性）。
    #[test]
    fn test_macos_enumerate_reads_descriptors() {
        let vid: u16 = 0x04E8;
        let pid_locked: u16 = 0x61FC;

        let summaries = MacOsDiscovery::enumerate(vid, pid_locked).expect("IOKit 只读侦察必须成功");
        if summaries.is_empty() {
            eprintln!("[跳过] 本机未接入 04e8:61fc 的 T7 Shield，无可断言的真实描述符");
            return;
        }

        for summary in &summaries {
            assert_eq!(summary.vid, vid);
            assert_eq!(summary.pid, pid_locked);
            assert_eq!(
                summary.alternate_settings.len(),
                2,
                "真机应为 1 个接口的 2 个备用设置（BOT 与 UAS）"
            );
            assert_eq!(summary.alternate_settings[0].protocol, 0x50);
            assert_eq!(summary.alternate_settings[1].protocol, 0x62);
            assert_eq!(summary.alternate_settings[0].class, 0x08);
            assert_eq!(summary.alternate_settings[0].subclass, 0x06);
        }

        let summary = &summaries[0];
        eprintln!(
            "[真机] 04e8:{pid_locked:04x} 描述符侦察：{} 个接口备用设置，alt0 proto {:#04x}（{} 个端点）、alt1 proto {:#04x}（{} 个端点）",
            summary.alternate_settings.len(),
            summary.alternate_settings[0].protocol,
            summary.alternate_settings[0].endpoints.len(),
            summary.alternate_settings[1].protocol,
            summary.alternate_settings[1].endpoints.len(),
        );
    }

    /// 未接入的 VID/PID 必须返回空列表（不 panic、不报错），即绝不误伤其它 USB 设备。
    #[test]
    fn test_macos_enumerate_ignores_other_vid_pid() {
        let summaries = MacOsDiscovery::enumerate(0x1234, 0x5678).expect("IOKit 只读侦察必须成功");
        assert!(summaries.is_empty(), "非目标 VID/PID 不得被枚举");
    }
}
