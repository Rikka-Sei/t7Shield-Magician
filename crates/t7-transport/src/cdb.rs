//! §4.3 CDB 逐字节构造：`cdb_security_in` / `cdb_security_out`。
//!
//! 本模块只按入参构造 CDB，不做任何协议判定：SECURITY PROTOCOL 字节固定 `0x01`（A 路，
//! D01/D12 排除其它协议路径），SP specific = BE16(ComID)（D03：ComID 由调用方从 Level-0
//! Discovery 运行时解析后传入），长度域为 BE32。

use std::time::Duration;

use crate::transport::ScsiCdb;

/// TCG 命令的响应分配长度（§4.3：固定 `0x00000800`，对齐官方 `GetTCGResponse` 缓冲）。
pub const TCG_ALLOC_LEN: u32 = 0x0000_0800;

/// Discovery 的响应分配长度（§4.3 例外：4096 B）。
pub const DISCOVERY_ALLOC_LEN: u32 = 0x0000_1000;

/// Discovery 的 SP specific（§4.2：固定 `0x0001`，唯一不使用运行时 ComID 的命令）。
pub const DISCOVERY_SP_SPECIFIC: u16 = 0x0001;

/// 单条 SCSI 命令超时（§6：30 s，与官方客户端 `w4 = 0x1e` 一致）。
pub const CMD_TIMEOUT: Duration = Duration::from_secs(30);

/// SECURITY PROTOCOL 字节：A 路 TCG Opal over USB（D01）。
const SECURITY_PROTOCOL_TCG: u8 = 0x01;

/// SECURITY PROTOCOL IN：`A2 01 <ComID BE16> 00 00 <alloc BE32> 00 00`。
pub fn cdb_security_in(comid: u16, alloc: u32) -> ScsiCdb {
    let mut cdb = [0u8; 12];
    cdb[0] = 0xA2;
    cdb[1] = SECURITY_PROTOCOL_TCG;
    cdb[2..4].copy_from_slice(&comid.to_be_bytes());
    cdb[6..10].copy_from_slice(&alloc.to_be_bytes());
    ScsiCdb(cdb)
}

/// SECURITY PROTOCOL OUT：`B5 01 <ComID BE16> 00 00 <len BE32> 00 00`。
pub fn cdb_security_out(comid: u16, len: u32) -> ScsiCdb {
    let mut cdb = [0u8; 12];
    cdb[0] = 0xB5;
    cdb[1] = SECURITY_PROTOCOL_TCG;
    cdb[2..4].copy_from_slice(&comid.to_be_bytes());
    cdb[6..10].copy_from_slice(&len.to_be_bytes());
    ScsiCdb(cdb)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CDB 黄金向量（附录 A3）：ComID 一律用测试局部变量（D03 禁止写成生产常量）。
    #[test]
    fn test_cdb_golden_vectors() {
        let observed_comid: u16 = 0x1004;

        assert_eq!(
            cdb_security_in(observed_comid, TCG_ALLOC_LEN).0,
            [0xA2, 0x01, 0x10, 0x04, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00]
        );
        assert_eq!(
            cdb_security_in(DISCOVERY_SP_SPECIFIC, DISCOVERY_ALLOC_LEN).0,
            [0xA2, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00]
        );
        assert_eq!(
            cdb_security_out(observed_comid, 128).0,
            [0xB5, 0x01, 0x10, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00]
        );
    }

    /// 固定域：只有偏移 0/1 的操作码与协议字节随 IN/OUT 变化，其余保留域恒为 0。
    #[test]
    fn test_cdb_fixed_fields_are_zero() {
        let cdb = cdb_security_out(0x0001, 0x0000_1234);
        assert_eq!(cdb.0[4], 0x00);
        assert_eq!(cdb.0[5], 0x00);
        assert_eq!(cdb.0[10], 0x00);
        assert_eq!(cdb.0[11], 0x00);
        assert_eq!(cdb.0[6..10], [0x00, 0x00, 0x12, 0x34]);
    }
}
