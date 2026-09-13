//! `SenseData` 与 sense/SCSI 完成状态 → [`TransportError`] 映射（跨平台纯函数）。
//!
//! sense 缓冲固定 32 字节（§4.4 契约；§6「禁止按响应内容动态扩容」）。固定格式（`0x70`/`0x71`）
//! 与描述符格式（`0x72`/`0x73`）都解析：`sense_key` 取低 4 位，ASC/ASCQ 分别取自各自偏移。

use crate::transport::TransportError;

/// sense 缓冲长度（§4.4 契约的固定值）。
pub const SENSE_BUFFER_LEN: usize = 32;

/// 固定格式 sense 数据读取 ASC/ASCQ 所需的最小长度（`0x0C`/`0x0D`）。
const FIXED_SENSE_MIN_LEN: usize = 14;

/// 描述符格式 sense 数据读取 ASC/ASCQ 所需的最小长度。
const DESCRIPTOR_SENSE_MIN_LEN: usize = 8;

/// SCSI 状态：GOOD。
pub const SCSI_STATUS_GOOD: u8 = 0x00;

/// SCSI 状态：CHECK CONDITION。
pub const SCSI_STATUS_CHECK_CONDITION: u8 = 0x02;

/// sense 记录（§4.4 契约字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SenseData {
    pub response_code: u8,
    pub sense_key: u8,
    pub asc: u8,
    pub ascq: u8,
}

impl SenseData {
    /// sense key `0x03` / ASC `0x11` / ASCQ `0x00`：设备不实现该 SCSI SECURITY PROTOCOL 通道
    /// （§5「判定链的关键区分」：0xFD 私有协议的判定依据，不是参数错误）。
    ///
    /// 本方法是该三元组判定的唯一定义处；协议层分类（`UnsupportedSecurityProtocol`）调用它。
    pub fn is_unsupported_security_protocol(&self) -> bool {
        self.sense_key == 0x03 && self.asc == 0x11 && self.ascq == 0x00
    }
}

/// 解析 sense 缓冲；缓冲内没有可识别的 sense 记录（全零 / 未知响应码 / 长度不足）时返回 `None`。
pub fn parse_sense(sbp: &[u8]) -> Option<SenseData> {
    let response_code = *sbp.first()?;
    match response_code & 0x7F {
        0x70 => {
            if sbp.len() < FIXED_SENSE_MIN_LEN {
                return None;
            }
            Some(SenseData {
                response_code,
                sense_key: sbp[2] & 0x0F,
                asc: sbp[12],
                ascq: sbp[13],
            })
        }
        0x72 => {
            if sbp.len() < DESCRIPTOR_SENSE_MIN_LEN {
                return None;
            }
            Some(SenseData {
                response_code,
                sense_key: sbp[1] & 0x0F,
                asc: sbp[2],
                ascq: sbp[3],
            })
        }
        _ => None,
    }
}

/// SCSI 完成状态 → 结果（§5）：GOOD 成功；CHECK CONDITION 包装 sense；其余完成状态走
/// [`TransportError::Platform`] 兜底并保留原始状态码（不做任何自动重放，D11）。
pub fn scsi_status_to_result(status: u8, sense: Option<SenseData>) -> Result<(), TransportError> {
    match status {
        SCSI_STATUS_GOOD => Ok(()),
        SCSI_STATUS_CHECK_CONDITION => match sense {
            Some(sense) => Err(TransportError::ScsiCheckCondition { sense }),
            // CHECK CONDITION 但 sense 无法解析：无可包装的 SenseData，按平台码兜底。
            None => Err(TransportError::Platform {
                code: i64::from(status),
            }),
        },
        other => Err(TransportError::Platform {
            code: i64::from(other),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_fixed_format_sense() {
        let mut sbp = [0u8; SENSE_BUFFER_LEN];
        sbp[0] = 0x70;
        sbp[2] = 0x03;
        sbp[12] = 0x11;
        sbp[13] = 0x00;
        let sense = parse_sense(&sbp).expect("固定格式 sense 必须被解析");
        assert_eq!(
            sense,
            SenseData {
                response_code: 0x70,
                sense_key: 0x03,
                asc: 0x11,
                ascq: 0x00,
            }
        );
        assert!(sense.is_unsupported_security_protocol());
    }

    #[test]
    fn test_parse_descriptor_format_sense() {
        let mut sbp = [0u8; SENSE_BUFFER_LEN];
        sbp[0] = 0x72;
        sbp[1] = 0x03;
        sbp[2] = 0x11;
        sbp[3] = 0x00;
        assert_eq!(
            parse_sense(&sbp),
            Some(SenseData {
                response_code: 0x72,
                sense_key: 0x03,
                asc: 0x11,
                ascq: 0x00,
            })
        );
    }

    /// 全零 sense（GOOD 命令后内核留下的空缓冲）与非法响应码都不得被当成 sense 记录。
    #[test]
    fn test_parse_sense_rejects_empty_and_unknown() {
        assert_eq!(parse_sense(&[0u8; SENSE_BUFFER_LEN]), None);
        let mut sbp = [0u8; SENSE_BUFFER_LEN];
        sbp[0] = 0x00;
        assert_eq!(parse_sense(&sbp), None);
        assert_eq!(parse_sense(&[]), None);
    }

    /// `03/11/00` 之外的三元组不得被判为通道不存在。
    #[test]
    fn test_unsupported_security_protocol_predicate() {
        let other = SenseData {
            response_code: 0x70,
            sense_key: 0x03,
            asc: 0x11,
            ascq: 0x01,
        };
        assert!(!other.is_unsupported_security_protocol());
    }

    #[test]
    fn test_scsi_status_to_result() {
        assert_eq!(scsi_status_to_result(SCSI_STATUS_GOOD, None), Ok(()));

        let sense = SenseData {
            response_code: 0x70,
            sense_key: 0x03,
            asc: 0x11,
            ascq: 0x00,
        };
        assert_eq!(
            scsi_status_to_result(SCSI_STATUS_CHECK_CONDITION, Some(sense)),
            Err(TransportError::ScsiCheckCondition { sense })
        );
        assert_eq!(
            scsi_status_to_result(SCSI_STATUS_CHECK_CONDITION, None),
            Err(TransportError::Platform { code: 0x02 })
        );
        // 0x08 = BUSY：既非 GOOD 也非 CHECK CONDITION → Platform 兜底，且不重试（D11）。
        assert_eq!(
            scsi_status_to_result(0x08, None),
            Err(TransportError::Platform { code: 0x08 })
        );
    }
}
