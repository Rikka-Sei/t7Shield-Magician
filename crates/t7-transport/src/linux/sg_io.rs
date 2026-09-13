//! `SG_IO` 薄层：自建 `#[repr(C)]` 结构 + 组装/判定（跨平台），只有真正调用 `ioctl` 的那一行
//! 被 `cfg(target_os = "linux")` 包住。
//!
//! 结构布局逐字段对齐 Linux `include/scsi/sg.h` 的 `struct sg_io_hdr`（64 位平台 88 字节），
//! 布局由 `#[cfg(test)]` 的偏移断言固化——偏移算错会把 ioctl 的参数解释成全然的另一种请求，
//! 是必须被拦住的错误。

use std::io;
use std::os::raw::{c_int, c_uint, c_void};
use std::os::unix::io::RawFd;
use std::time::Duration;

use crate::errno_map::map_sg_io_host_status;
use crate::sense::{parse_sense, scsi_status_to_result, SenseData, SENSE_BUFFER_LEN};
use crate::transport::{Direction, ScsiCdb, TransportError};

/// `SG_IO`：`_IOC(写|读, 'S', 0x85, sizeof(struct sg_io_hdr))` 在 Linux 上的取值。
pub const SG_IO: u32 = 0x2285;

/// `interface_id` 字段必须填的首字节（`struct sg_io_hdr` 的 `'S'`）。
pub const SG_INTERFACE_ID: c_int = b'S' as c_int;

/// 数据相方向：不传输数据（本 crate 不使用，保留完整常量集合供判定）。
pub const SG_DXFER_NONE: c_int = -1;

/// 数据相方向：主机 → 设备。
pub const SG_DXFER_TO_DEV: c_int = -2;

/// 数据相方向：设备 → 主机。
pub const SG_DXFER_FROM_DEV: c_int = -3;

/// Linux `struct sg_io_hdr`（`include/scsi/sg.h`）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SgIoHdr {
    /// `'S'`。
    pub interface_id: c_int,
    /// `SG_DXFER_*`。
    pub dxfer_direction: c_int,
    /// CDB 长度（固定 12）。
    pub cmd_len: u8,
    /// sense 缓冲长度（固定 [`SENSE_BUFFER_LEN`]）。
    pub mx_sb_len: u8,
    /// SG_IO 向量数（本实现固定 0：只用单块 `dxferp`）。
    pub iovec_count: u16,
    /// 数据相长度。
    pub dxfer_len: c_uint,
    /// 数据相缓冲。
    pub dxferp: *mut c_void,
    /// CDB 缓冲。
    pub cmdp: *mut u8,
    /// sense 缓冲。
    pub sbp: *mut u8,
    /// 超时（毫秒）。
    pub timeout: c_uint,
    /// SG 层标志（本实现固定 0）。
    pub flags: c_uint,
    /// 包标识（本实现固定 0）。
    pub pack_id: c_int,
    /// 用户指针（本实现固定 `null`）。
    pub usr_ptr: *mut c_void,
    /// SCSI 状态字节。
    pub status: u8,
    /// 掩码后的状态。
    pub masked_status: u8,
    /// 消息状态。
    pub msg_status: u8,
    /// 实际写入 sense 缓冲的字节数。
    pub sb_len_wr: u8,
    /// SCSI 层主机状态（`DID_*`）。
    pub host_status: u16,
    /// 驱动状态（`DRIVER_*`）。
    pub driver_status: u16,
    /// 未传输的字节数。
    pub resid: c_int,
    /// 命令耗时（毫秒，由内核回填）。
    pub duration: c_uint,
    /// 附加信息（由内核回填）。
    pub info: c_uint,
}

impl SgIoHdr {
    /// 装配 sense 缓冲指针（必须在 `submit` 之前调用；调用方保证缓冲生命周期覆盖整个 ioctl）。
    pub fn attach_sense(&mut self, sense: &mut [u8; SENSE_BUFFER_LEN]) {
        self.sbp = sense.as_mut_ptr();
    }
}

/// 组装一次 `SG_IO` 调用所需的头（跨平台可构造、可测；不做任何 syscall）。
///
/// `mx_sb_len` 固定为 [`SENSE_BUFFER_LEN`]（§4.4 的 32 字节 sense 缓冲），sense 指针由
/// [`SgIoHdr::attach_sense`] 装配；超时按毫秒写入（30 s → 30000）。
pub fn build_sg_io_hdr(
    cdb: &ScsiCdb,
    direction: Direction,
    data: &mut [u8],
    timeout: Duration,
) -> SgIoHdr {
    SgIoHdr {
        interface_id: SG_INTERFACE_ID,
        dxfer_direction: match direction {
            Direction::In => SG_DXFER_FROM_DEV,
            Direction::Out => SG_DXFER_TO_DEV,
        },
        cmd_len: cdb.0.len() as u8,
        mx_sb_len: SENSE_BUFFER_LEN as u8,
        iovec_count: 0,
        dxfer_len: u32::try_from(data.len()).unwrap_or(u32::MAX),
        dxferp: data.as_mut_ptr().cast::<c_void>(),
        cmdp: cdb.0.as_ptr().cast_mut(),
        sbp: std::ptr::null_mut(),
        timeout: u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX),
        flags: 0,
        pack_id: 0,
        usr_ptr: std::ptr::null_mut(),
        status: 0,
        masked_status: 0,
        msg_status: 0,
        sb_len_wr: 0,
        host_status: 0,
        driver_status: 0,
        resid: 0,
        duration: 0,
        info: 0,
    }
}

/// 判定一次 `SG_IO` 的完成结果（跨平台纯函数）：主机状态/超时优先，其次 SCSI 状态与 sense，
/// 成功时返回实际传输字节数（`dxfer_len - resid`）。
pub fn check_completion(
    hdr: &SgIoHdr,
    sense: Option<SenseData>,
    elapsed: Duration,
) -> Result<usize, TransportError> {
    if let Some(error) = map_sg_io_host_status(hdr.host_status, elapsed) {
        return Err(error);
    }
    scsi_status_to_result(hdr.status, sense)?;
    let resid = u32::try_from(hdr.resid).unwrap_or(0);
    Ok(usize::try_from(hdr.dxfer_len.saturating_sub(resid)).unwrap_or(usize::MAX))
}

/// 下发 `SG_IO`（数据相缓冲与 sense 缓冲由 `hdr` 指向）。
///
/// Linux 上是 [`libc::ioctl`] 的一次调用；非 Linux 平台没有该 ioctl，直接返回 `ENOSYS`
/// （由此把「平台通道不存在」的事实交给 errno 映射，而不是在别处硬编码平台判定）。
#[cfg(target_os = "linux")]
pub fn submit(fd: RawFd, hdr: &mut SgIoHdr) -> io::Result<()> {
    // SAFETY: fd 来自 open 成功的设备节点；hdr 是 #[repr(C)] 的 sg_io_hdr 且在本调用内一直有效。
    let rc = unsafe { libc::ioctl(fd, SG_IO as libc::c_ulong, hdr as *mut SgIoHdr) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// 非 Linux 平台：没有 `SG_IO`。
#[cfg(not(target_os = "linux"))]
pub fn submit(_fd: RawFd, _hdr: &mut SgIoHdr) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::ENOSYS))
}

/// 解析 sense 缓冲（供调用方在判定之前记录 `last_sense`）。
pub fn sense_from_buffer(sense: &[u8; SENSE_BUFFER_LEN]) -> Option<SenseData> {
    parse_sense(sense)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdb::{cdb_security_in, CMD_TIMEOUT, TCG_ALLOC_LEN};

    /// 布局核对：`struct sg_io_hdr` 在 64 位平台的字段偏移与总长（88 字节）。
    #[test]
    fn test_sg_io_hdr_layout_matches_kernel_struct() {
        assert_eq!(std::mem::size_of::<SgIoHdr>(), 88);
        assert_eq!(std::mem::offset_of!(SgIoHdr, interface_id), 0);
        assert_eq!(std::mem::offset_of!(SgIoHdr, cmd_len), 8);
        assert_eq!(std::mem::offset_of!(SgIoHdr, mx_sb_len), 9);
        assert_eq!(std::mem::offset_of!(SgIoHdr, iovec_count), 10);
        assert_eq!(std::mem::offset_of!(SgIoHdr, dxfer_len), 12);
        assert_eq!(std::mem::offset_of!(SgIoHdr, dxferp), 16);
        assert_eq!(std::mem::offset_of!(SgIoHdr, cmdp), 24);
        assert_eq!(std::mem::offset_of!(SgIoHdr, sbp), 32);
        assert_eq!(std::mem::offset_of!(SgIoHdr, timeout), 40);
        assert_eq!(std::mem::offset_of!(SgIoHdr, flags), 44);
        assert_eq!(std::mem::offset_of!(SgIoHdr, pack_id), 48);
        assert_eq!(std::mem::offset_of!(SgIoHdr, usr_ptr), 56);
        assert_eq!(std::mem::offset_of!(SgIoHdr, status), 64);
        assert_eq!(std::mem::offset_of!(SgIoHdr, sb_len_wr), 67);
        assert_eq!(std::mem::offset_of!(SgIoHdr, host_status), 68);
        assert_eq!(std::mem::offset_of!(SgIoHdr, driver_status), 70);
        assert_eq!(std::mem::offset_of!(SgIoHdr, resid), 72);
        assert_eq!(std::mem::offset_of!(SgIoHdr, duration), 76);
        assert_eq!(std::mem::offset_of!(SgIoHdr, info), 80);
    }

    /// 头字段填充（跨平台无条件运行）：方向、长度、指针、超时。
    #[test]
    fn test_build_sg_io_hdr_fields() {
        let cdb = cdb_security_in(0x1004, TCG_ALLOC_LEN);
        let mut data = [0u8; TCG_ALLOC_LEN as usize];
        let data_ptr = data.as_mut_ptr().cast::<c_void>();

        let hdr = build_sg_io_hdr(&cdb, Direction::In, &mut data, CMD_TIMEOUT);
        assert_eq!(hdr.interface_id, b'S' as c_int);
        assert_eq!(hdr.dxfer_direction, SG_DXFER_FROM_DEV);
        assert_eq!(hdr.cmd_len, 12);
        assert_eq!(hdr.mx_sb_len, 32);
        assert_eq!(hdr.dxfer_len, TCG_ALLOC_LEN);
        assert_eq!(hdr.dxferp, data_ptr);
        assert_eq!(hdr.cmdp, cdb.0.as_ptr().cast_mut());
        assert_eq!(hdr.timeout, 30_000);
        assert_eq!(hdr.iovec_count, 0);
        assert_eq!(hdr.flags, 0);
        assert!(hdr.sbp.is_null(), "sense 缓冲由 attach_sense 装配");

        let mut sense = [0u8; SENSE_BUFFER_LEN];
        let mut hdr = hdr;
        hdr.attach_sense(&mut sense);
        assert_eq!(hdr.sbp, sense.as_mut_ptr());

        let mut out_data = [0u8; 128];
        let out_hdr = build_sg_io_hdr(&cdb, Direction::Out, &mut out_data, CMD_TIMEOUT);
        assert_eq!(
            out_hdr.dxfer_direction, SG_DXFER_TO_DEV,
            "OUT 方向为 TO_DEV"
        );
        assert_eq!(out_hdr.dxfer_len, 128);
    }

    /// 完成判定：主机状态超时优先、CHECK CONDITION 包装 sense、GOOD 返回 `dxfer_len - resid`。
    #[test]
    fn test_check_completion_maps_timeout_status_and_resid() {
        let cdb = cdb_security_in(0x1004, TCG_ALLOC_LEN);
        let mut data = [0u8; TCG_ALLOC_LEN as usize];
        let mut hdr = build_sg_io_hdr(&cdb, Direction::In, &mut data, CMD_TIMEOUT);
        let elapsed = Duration::from_secs(30);

        hdr.dxfer_len = 96;
        hdr.resid = 40;
        assert_eq!(
            check_completion(&hdr, None, elapsed),
            Ok(56),
            "传输字节数 = dxfer_len - resid"
        );

        hdr.host_status = crate::errno_map::SG_HOST_DID_TIME_OUT;
        assert_eq!(
            check_completion(&hdr, None, elapsed),
            Err(TransportError::Timeout {
                elapsed: Duration::from_secs(30)
            }),
            "主机状态超时优先于 SCSI 状态"
        );

        hdr.host_status = 0;
        hdr.status = 0x02;
        let sense = SenseData {
            response_code: 0x70,
            sense_key: 0x03,
            asc: 0x11,
            ascq: 0x00,
        };
        assert_eq!(
            check_completion(&hdr, Some(sense), elapsed),
            Err(TransportError::ScsiCheckCondition { sense })
        );
        assert!(sense.is_unsupported_security_protocol());
    }

    /// sense 回填路径：32 字节缓冲经 [`sense_from_buffer`] 解析出 key/ASC/ASCQ。
    #[test]
    fn test_sense_from_buffer_roundtrip() {
        let mut sense = [0u8; SENSE_BUFFER_LEN];
        sense[0] = 0x70;
        sense[2] = 0x03;
        sense[12] = 0x11;
        assert_eq!(
            sense_from_buffer(&sense),
            Some(SenseData {
                response_code: 0x70,
                sense_key: 0x03,
                asc: 0x11,
                ascq: 0x00,
            })
        );
        assert_eq!(sense_from_buffer(&[0u8; SENSE_BUFFER_LEN]), None);
    }
}
