//! §5 错误模型：`ProtocolError` 全变体，以及进度步骤 `UnlockStep` 与判据错误归因
//! 用的 `CommandStep`。
//!
//! 诊断文本（`Display`）只描述事实与步骤，不含口令内容与长度（§6 安全约束）。

use std::fmt;

use t7_transport::sense::SenseData;

/// §4.13 的进度步骤：与 §4.7 的 7 条命令一一对应（Discovery 与 StartSession 之后）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnlockStep {
    /// `FB 00` + 状态列表。
    StartTransaction,
    /// `Set(MBRCONTROL, 列 2, 1)`。
    SetMbrDone,
    /// `Set(LOCKINGRANGE_GLOBAL, 列 7, 0)`。
    SetReadLocked,
    /// `Set(LOCKINGRANGE_GLOBAL, 列 8, 0)`。
    SetWriteLocked,
    /// `Set(DATASTORE, 行 2, {0x03})`。
    SetDataStoreRow2,
    /// `FC 00` + 状态列表。
    EndTransaction,
    /// `FA` + 状态列表。
    EndSession,
}

impl UnlockStep {
    /// §4.7 的 7 条命令顺序，也就是进度上报顺序。
    pub const ALL: [UnlockStep; 7] = [
        UnlockStep::StartTransaction,
        UnlockStep::SetMbrDone,
        UnlockStep::SetReadLocked,
        UnlockStep::SetWriteLocked,
        UnlockStep::SetDataStoreRow2,
        UnlockStep::EndTransaction,
        UnlockStep::EndSession,
    ];
}

impl fmt::Display for UnlockStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            UnlockStep::StartTransaction => "StartTransaction",
            UnlockStep::SetMbrDone => "SetMbrDone",
            UnlockStep::SetReadLocked => "SetReadLocked",
            UnlockStep::SetWriteLocked => "SetWriteLocked",
            UnlockStep::SetDataStoreRow2 => "SetDataStoreRow2",
            UnlockStep::EndTransaction => "EndTransaction",
            UnlockStep::EndSession => "EndSession",
        };
        f.write_str(name)
    }
}

/// §5 判据错误归因到的命令。
///
/// §4.13 的 `UnlockStep` 只覆盖 §4.7 的 7 条命令（用于进度上报）；而 §5 的长度判据
/// （`UnexpectedResponseLength`）也覆盖 StartSession（期望 `data_len = 37`），该命令
/// 不产生进度上报，故在此单列。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandStep {
    /// §4.6 的会话建立命令（`data_len` 期望 37）。
    StartSession,
    /// §4.7 的 7 条命令之一。
    Unlock(UnlockStep),
}

impl From<UnlockStep> for CommandStep {
    fn from(step: UnlockStep) -> Self {
        CommandStep::Unlock(step)
    }
}

impl fmt::Display for CommandStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommandStep::StartSession => f.write_str("StartSession"),
            CommandStep::Unlock(step) => write!(f, "{step}"),
        }
    }
}

/// §5 的协议层错误分类（唯一权威定义；变体与语义逐条对应 §5 错误表）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// Discovery 响应长度 < `0x31`。
    DiscoveryTooShort { len: usize },
    /// 描述符区没有可用的 Opal SSC 描述符（feature `0x0203`），或该描述符取不到 ComID。
    NoOpalSscDescriptor,
    /// 有 Opal SSC 描述符但缺 Locking 描述符（feature `0x0002`），不得推断锁定状态。
    LockingDescriptorMissing,
    /// 请求的协议字节不是 `0x01`，或设备回 sense `03/11/00`。
    UnsupportedSecurityProtocol { proto: u8, sense: SenseData },
    /// 状态列表内方法状态字节非 0（口令错误实测为 `1`）。
    SessionRejected { status_byte: u8 },
    /// `Set` 类命令应答 `data_len = 0` 且无状态列表（会话号错位的信号，判致命）。
    EmptyResponse { step: CommandStep },
    /// 应答长度不等于 §4.6/§4.7 的期望值（37 / 2 / 8 / 1），或应答形状不符（长度对但缺状态列表）。
    UnexpectedResponseLength {
        step: CommandStep,
        expected: usize,
        actual: usize,
    },
    /// StartSession 应答中 token[4]/token[5] 缺失或类型不符（无数值）。
    SessionIdsMissing,
    /// 口令设置 / 修改 / 删除在字节级证据齐备前不可执行（§4.11）。
    PasswordOperationUnspecified,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::DiscoveryTooShort { len } => {
                write!(f, "discovery 响应过短：{len} 字节（至少需要 0x31）")
            }
            ProtocolError::NoOpalSscDescriptor => f.write_str("描述符区无可用 Opal SSC 描述符"),
            ProtocolError::LockingDescriptorMissing => {
                f.write_str("缺 Locking 描述符，无法判定锁定状态")
            }
            ProtocolError::UnsupportedSecurityProtocol { proto, sense } => write!(
                f,
                "不支持的 SECURITY PROTOCOL {proto:#04x}（sense {:02x}/{:02x}/{:02x}）",
                sense.sense_key, sense.asc, sense.ascq
            ),
            ProtocolError::SessionRejected { status_byte } => {
                write!(f, "会话被设备拒绝（方法状态字节 {status_byte}）")
            }
            ProtocolError::EmptyResponse { step } => write!(f, "{step} 回空应答（会话号错位信号）"),
            ProtocolError::UnexpectedResponseLength {
                step,
                expected,
                actual,
            } => write!(
                f,
                "{step} 应答长度不符：期望 {expected} 字节，实际 {actual} 字节"
            ),
            ProtocolError::SessionIdsMissing => f.write_str("应答缺少可用的会话号原子"),
            ProtocolError::PasswordOperationUnspecified => {
                f.write_str("口令写操作的字节级序列尚未证实，不可执行")
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// 进度步骤必须是 §4.13 的 7 个变体，顺序与 §4.7 的 7 条命令一致。
    #[test]
    fn test_unlock_step_order_is_the_seven_commands() {
        assert_eq!(UnlockStep::ALL.len(), 7);
        assert_eq!(UnlockStep::ALL[0], UnlockStep::StartTransaction);
        assert_eq!(UnlockStep::ALL[3], UnlockStep::SetWriteLocked);
        assert_eq!(UnlockStep::ALL[6], UnlockStep::EndSession);
        assert_eq!(
            CommandStep::from(UnlockStep::SetMbrDone),
            CommandStep::Unlock(UnlockStep::SetMbrDone)
        );
        assert_eq!(CommandStep::StartSession.to_string(), "StartSession");
    }
}
