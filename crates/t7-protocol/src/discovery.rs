//! §4.1/§4.2：设备识别（REQ-001）与 Level-0 Discovery 解析（REQ-002）。
//!
//! 本模块不做 I/O：调用方用 [`discovery_cdb`] 取 12 字节 CDB、经 `Transport` 下发，再把
//! 响应交给 [`parse_level0`]。ComID 只从 Opal SSC 描述符（feature `0x0203`）运行时解析，
//! 锁定状态只从 Locking 描述符（feature `0x0002`）读取；两者缺失都返回 §5 的对应错误，
//! 绝不用「未锁定」之类的默认值兜底。

use crate::error::ProtocolError;
use t7_transport::cdb::{cdb_security_in, DISCOVERY_ALLOC_LEN, DISCOVERY_SP_SPECIFIC};
use t7_transport::transport::ScsiCdb;

/// §4.1：Samsung `idVendor`。
pub const VENDOR_ID: u16 = 0x04e8;
/// §4.1：锁定态 `idProduct`（设备只暴露 34.5 MB 影子安全区）。
pub const PID_LOCKED: u16 = 0x61fc;
/// §4.1：解锁态 `idProduct`（设备暴露真实分区表）。
pub const PID_UNLOCKED: u16 = 0x61fb;

/// 描述符区起点：Level-0 响应的前 `0x30` 字节为缓冲头。
const DESCRIPTOR_START: usize = 0x30;
/// 描述符项头长度：BE16 feature + u8 version + u8 len。
const DESCRIPTOR_HEADER_LEN: usize = 4;
/// §4.2：可解析响应的最小长度（描述符区起点 + 1 字节）；更短即 `DiscoveryTooShort`。
const MIN_RESPONSE_LEN: usize = DESCRIPTOR_START + 1;
/// §4.2：终止项 feature，命中即停止遍历，且不记入描述符。
const FEATURE_TERMINATOR: u16 = 0x0000;
/// §4.2：Locking 特性，描述符数据第 1 字节为 flags。
const FEATURE_LOCKING: u16 = 0x0002;
/// §4.2：Opal SSC V2.00 特性，描述符数据前 2 字节按 BE 解释为 ComID。
const FEATURE_OPAL_SSC_V2: u16 = 0x0203;

/// §3.2 设备态：由 USB 描述符扫描结果与 Locking flags 共同裁决。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    /// 锁定态：PID `0x61fc`，Locking flags bit2 = 1。
    Locked,
    /// 解锁态：PID `0x61fb`，Locking flags bit2 = 0 且 bit5 = 1。
    Unlocked,
    /// 重枚举窗口内：解锁序列已收尾，PID 尚未稳定为新值或设备节点暂时不存在；
    /// 不由 PID 判定，保留给设备态模型的第三态。
    ReEnumerating,
}

/// §4.2 特性描述符：项头 `{BE16 feature, u8 version, u8 len}` + `len` 字节数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureDescriptor {
    /// 特性号，例如 `0x0203` = Opal SSC V2.00、`0x0002` = Locking。
    pub feature: u16,
    /// 特性版本字节。
    pub version: u8,
    /// 特性数据；`data` 的起点即描述符起点 `+4`。
    pub data: Vec<u8>,
}

/// §3.2/§4.2 Locking flags 的原始字节。
///
/// `raw == 0` 表示尚未取得 flags（未做 discovery，或响应缺 Locking 描述符），
/// 禁止当作「未锁定」判据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockingFlags {
    /// flags 字节原文。
    pub raw: u8,
}

impl LockingFlags {
    /// bit2：设备处于锁定态。
    pub fn locked(&self) -> bool {
        self.raw & 0x04 != 0
    }

    /// bit4：MBR 已启用。
    pub fn mbr_enabled(&self) -> bool {
        self.raw & 0x10 != 0
    }

    /// bit5：MBR 已完成。
    pub fn mbr_done(&self) -> bool {
        self.raw & 0x20 != 0
    }
}

/// §4.2 Level-0 Discovery 解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    /// Opal SSC V2.00 描述符 `+4` 的 BE16 值（运行时解析所得，不写死实测值）。
    pub base_comid: u16,
    /// Locking 描述符 `+4` 的 flags 字节。
    pub locking: LockingFlags,
    /// 按出现顺序保留的全部描述符（含 `0x0002`/`0x0203` 等，不含终止项）。
    pub descriptors: Vec<FeatureDescriptor>,
}

/// §4.1：按 USB `vid`/`pid` 判定设备态。
///
/// `idVendor` 必须为 [`VENDOR_ID`]；`idProduct` 为 [`PID_LOCKED`] → `Locked`、
/// [`PID_UNLOCKED`] → `Unlocked`，其余（含厂商不符）一律 `None`——不识别为 T7 Shield
/// 时不得发送任何 SCSI 命令。`ReEnumerating` 不由 PID 判定。
pub fn identify_device(vid: u16, pid: u16) -> Option<DeviceState> {
    if vid != VENDOR_ID {
        return None;
    }
    match pid {
        PID_LOCKED => Some(DeviceState::Locked),
        PID_UNLOCKED => Some(DeviceState::Unlocked),
        _ => None,
    }
}

/// §4.2：解析 Level-0 Discovery 响应。
///
/// 从偏移 `0x30` 起逐项解析 `{BE16 feature, u8 version, u8 len}` + `len` 字节数据，步长
/// `4 + len`。命中任一终止条件即停止遍历，且不把终止项记入 `descriptors`：① feature 为
/// `0x0000`；② 当前位置到缓冲末尾不足 4 字节；③ `4 + len` 越过缓冲末尾。
///
/// 遍历结束后解析两个关键描述符：`base_comid` 取 feature `0x0203` 描述符数据的前 2 字节
/// （BE16），`locking.raw` 取 feature `0x0002` 描述符数据的第 1 字节。`0x0203` 缺失或
/// 取不到 ComID → [`ProtocolError::NoOpalSscDescriptor`]；`0x0002` 缺失或取不到 flags →
/// [`ProtocolError::LockingDescriptorMissing`]（不得推断锁定状态）。
pub fn parse_level0(buf: &[u8]) -> Result<Discovery, ProtocolError> {
    if buf.len() < MIN_RESPONSE_LEN {
        return Err(ProtocolError::DiscoveryTooShort { len: buf.len() });
    }

    let mut descriptors = Vec::new();
    let mut pos = DESCRIPTOR_START;
    while pos + DESCRIPTOR_HEADER_LEN <= buf.len() {
        let feature = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        if feature == FEATURE_TERMINATOR {
            break;
        }
        let version = buf[pos + 2];
        let len = usize::from(buf[pos + 3]);
        let data_start = pos + DESCRIPTOR_HEADER_LEN;
        let data_end = data_start + len;
        if data_end > buf.len() {
            break;
        }
        descriptors.push(FeatureDescriptor {
            feature,
            version,
            data: buf[data_start..data_end].to_vec(),
        });
        pos = data_end;
    }

    let opal = descriptors
        .iter()
        .find(|item| item.feature == FEATURE_OPAL_SSC_V2)
        .ok_or(ProtocolError::NoOpalSscDescriptor)?;
    if opal.data.len() < 2 {
        return Err(ProtocolError::NoOpalSscDescriptor);
    }
    let base_comid = u16::from_be_bytes([opal.data[0], opal.data[1]]);

    let locking = descriptors
        .iter()
        .find(|item| item.feature == FEATURE_LOCKING)
        .ok_or(ProtocolError::LockingDescriptorMissing)?;
    let raw = *locking
        .data
        .first()
        .ok_or(ProtocolError::LockingDescriptorMissing)?;

    Ok(Discovery {
        base_comid,
        locking: LockingFlags { raw },
        descriptors,
    })
}

/// §4.2：discovery 用的 SECURITY PROTOCOL IN——固定 SP specific `0x0001`、分配长度
/// 4096 B，即 `A2 01 00 01 00 00 00 00 10 00 00 00`。
///
/// 字节构造归 `t7-transport::cdb`，本函数只做一次调用，不在协议层复写 CDB 字节。
pub fn discovery_cdb() -> ScsiCdb {
    cdb_security_in(DISCOVERY_SP_SPECIFIC, DISCOVERY_ALLOC_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ProtocolError;

    /// 十六进制字面量 → 字节向量（输入须为偶数个十六进制字符）。
    fn hex(text: &str) -> Vec<u8> {
        assert_eq!(text.len() % 2, 0, "十六进制字面量长度必须为偶数：{text}");
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("十六进制字面量"))
            .collect()
    }

    /// 构造 Level-0 响应：8 字节缓冲头 + 补零到 `0x30` + 给定描述符区。
    fn level0_response(descriptor_area: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&hex("000000a000000001"));
        buf.resize(DESCRIPTOR_START, 0x00);
        buf.extend_from_slice(descriptor_area);
        buf
    }

    /// 真机 fixture（`../t7Shield-protocol/analysis/probe-linux.raw`，经参考实现 selftest
    /// 固化）：ComID 必须从 Opal SSC 描述符解析、锁定态由 Locking flags 读出、遍历在
    /// `0x0000` 终止项处停止。同测例附带 §4.2 的三条边界断言。
    #[test]
    fn test_level0_parses_base_comid() {
        let mut fixture = Vec::new();
        fixture.extend_from_slice(&hex("000000a000000001")); // 8 字节缓冲头
        fixture.extend(std::iter::repeat_n(0x00, 0x28)); // 补齐到 0x30
        fixture.extend_from_slice(&hex("0001100c110000000000000000000000"));
        fixture.extend_from_slice(&hex("0002100c1f0000000000000000000000"));
        fixture.extend_from_slice(&hex(
            "0003101c01000000000000000000000200000000000000080000000000000000",
        ));
        fixture.extend_from_slice(&hex("0202100c0000000900a0000000000001"));
        fixture.extend_from_slice(&hex("0203101010040001000004000900000000000000"));
        fixture.extend_from_slice(&hex("0402100c010000000000000000000000"));
        fixture.extend(std::iter::repeat_n(0x00, 16)); // 末尾填充（含 0xa4 的 0x0000 终止项）
        assert_eq!(fixture.len(), 0xb4);

        let discovery = parse_level0(&fixture).expect("真机 fixture 必须解析成功");

        // 实测值只作测试局部变量：生产路径不得出现该数值。
        let observed_comid: u16 = 0x1004;
        assert_eq!(discovery.base_comid, observed_comid);
        assert_eq!(discovery.locking.raw, 0x1f);
        assert!(discovery.locking.locked());
        assert!(discovery.locking.mbr_enabled());
        assert!(!discovery.locking.mbr_done());

        let features: Vec<u16> = discovery.descriptors.iter().map(|d| d.feature).collect();
        assert_eq!(
            features,
            vec![0x0001, 0x0002, 0x0003, 0x0202, 0x0203, 0x0402],
            "终止项 0x0000 不得进入 descriptors"
        );
        assert!(discovery.descriptors.iter().all(|d| d.version == 0x10));

        let opal = discovery
            .descriptors
            .iter()
            .find(|d| d.feature == 0x0203)
            .expect("Opal SSC 描述符");
        assert_eq!(opal.data.len(), 16);
        assert_eq!(opal.data[0..2], [0x10, 0x04]);

        // 边界一：响应长度不足 0x31（16 字节，以及恰好 0x30 字节）。
        assert_eq!(
            parse_level0(&[0u8; 16]),
            Err(ProtocolError::DiscoveryTooShort { len: 16 })
        );
        assert_eq!(
            parse_level0(&[0u8; DESCRIPTOR_START]),
            Err(ProtocolError::DiscoveryTooShort {
                len: DESCRIPTOR_START
            })
        );

        // 边界二：只有 TPer 与 Locking 描述符 → 无 Opal SSC 描述符。
        let mut tper_locking = hex("0001100c110000000000000000000000");
        tper_locking.extend_from_slice(&hex("0002100c1f0000000000000000000000"));
        tper_locking.extend_from_slice(&hex("00000000")); // 终止项
        assert_eq!(
            parse_level0(&level0_response(&tper_locking)),
            Err(ProtocolError::NoOpalSscDescriptor)
        );

        // 边界三：有 Opal SSC 描述符但缺 Locking → 不得推断锁定状态。
        let mut opal_only = hex("0203101010040001000004000900000000000000");
        opal_only.extend_from_slice(&hex("00000000")); // 终止项
        assert_eq!(
            parse_level0(&level0_response(&opal_only)),
            Err(ProtocolError::LockingDescriptorMissing)
        );
    }

    /// §4.1：非 T7 Shield 的 PID 与厂商一律拒绝，`ReEnumerating` 不由 PID 判定。
    #[test]
    fn test_unknown_pid_is_rejected() {
        assert_eq!(
            (VENDOR_ID, PID_LOCKED, PID_UNLOCKED),
            (0x04e8, 0x61fc, 0x61fb)
        );
        assert_eq!(identify_device(0x04e8, 0x61fc), Some(DeviceState::Locked));
        assert_eq!(identify_device(0x04e8, 0x61fb), Some(DeviceState::Unlocked));
        assert_eq!(identify_device(0x04e8, 0x61ff), None);
        assert_eq!(identify_device(0x1234, 0x61fc), None);
    }

    /// §4.2：三个终止条件各自停止遍历，且保留此前解析出的描述符。
    #[test]
    fn test_termination_conditions_stop_parsing() {
        let mut area = hex("0001100c110000000000000000000000");
        area.extend_from_slice(&hex("0002100c1f0000000000000000000000"));
        area.extend_from_slice(&hex("0203101010040001000004000900000000000000"));
        let base = level0_response(&area);

        let parsed = parse_level0(&base).expect("描述符区恰好结束时必须解析成功");
        assert_eq!(parsed.descriptors.len(), 3);
        assert_eq!(parsed.base_comid, 0x1004);

        // ② 尾部剩余 0 / 1 / 3 字节：不足一个项头，停止遍历且不产生新描述符。
        for tail in ["", "00", "000210"] {
            let mut buf = base.clone();
            buf.extend_from_slice(&hex(tail));
            let parsed = parse_level0(&buf).expect("尾部不足项头时必须停止遍历");
            assert_eq!(parsed.descriptors.len(), 3, "尾部 {tail:?} 不得产生描述符");
        }

        // ③ `4 + len` 越界：声明 12 字节数据但只给 4 字节，停止且不读取越界部分。
        let mut truncated = base.clone();
        truncated.extend_from_slice(&hex("0402100c01000000"));
        let parsed = parse_level0(&truncated).expect("长度越界的描述符必须停止遍历");
        assert_eq!(parsed.descriptors.len(), 3);

        // ① 终止项 `0x0000`：停止遍历，且其后合法形态的项也不再解析。
        let mut terminated = base.clone();
        terminated.extend_from_slice(&hex("00000004deadbeef")); // 终止项（含 4 字节数据）
        terminated.extend_from_slice(&hex("0402100c010000000000000000000000"));
        let parsed = parse_level0(&terminated).expect("终止项必须停止遍历");
        let features: Vec<u16> = parsed.descriptors.iter().map(|d| d.feature).collect();
        assert_eq!(features, vec![0x0001, 0x0002, 0x0203]);
        assert_eq!(parsed.locking.raw, 0x1f);
    }

    /// §4.2 Locking flags 位语义：bit2 = Locked、bit4 = MBREnabled、bit5 = MBRDone；
    /// 含真机锁定态 `0x1f` 与解锁态 `0x3b` 两组实测取值。
    #[test]
    fn test_locking_flags_bits() {
        // 位隔离：三个掩码互不重叠，单个真位只翻转对应判据。
        let cases = [
            (0x00, false, false, false),
            (0x04, true, false, false),
            (0x10, false, true, false),
            (0x20, false, false, true),
        ];
        for (raw, locked, mbr_enabled, mbr_done) in cases {
            let flags = LockingFlags { raw };
            assert_eq!(flags.locked(), locked, "raw {raw:#04x} 的 bit2");
            assert_eq!(flags.mbr_enabled(), mbr_enabled, "raw {raw:#04x} 的 bit4");
            assert_eq!(flags.mbr_done(), mbr_done, "raw {raw:#04x} 的 bit5");
        }

        let locked = LockingFlags { raw: 0x1f };
        assert!(locked.locked());
        assert!(locked.mbr_enabled());
        assert!(!locked.mbr_done());

        let unlocked = LockingFlags { raw: 0x3b };
        assert!(!unlocked.locked());
        assert!(unlocked.mbr_enabled());
        assert!(unlocked.mbr_done());
    }

    /// §4.2：discovery 的 CDB 逐字节固定为 `A2 01 00 01 00 00 00 00 10 00 00 00`。
    #[test]
    fn test_discovery_cdb_bytes() {
        assert_eq!(
            discovery_cdb().0,
            [0xa2, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00]
        );
    }
}
