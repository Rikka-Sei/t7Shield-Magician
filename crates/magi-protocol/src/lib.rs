//! magi-protocol：T7 Shield 协议层（纯协议逻辑，零 UI 依赖）。
//!
//! 依赖方向：`magi-app` → `magi-protocol` → `magi-transport`（反向禁止）。本 crate 负责 Level-0
//! Discovery 解析、TCG 帧构造与响应解析、会话状态机与解锁判据。所有协议行为以
//! `docs/specs/t7-magician/spec.md` 为唯一权威；本层不做平台 I/O，也不定义 CDB 字节
//! （CDB 归 `magi-transport::cdb`，本层只用 `Transport` trait 收发）。
//!
//! 安全约束：口令缓冲归本 crate 所有（`Password(Zeroizing<Vec<u8>>)`），报文构造完成后
//! 立即 zeroize 并在 `Drop` 二次清零；口令内容与长度都不写日志。
//!
//! 对外入口见文末的再导出；模块本身也可按 `magi_protocol::<module>::…` 直接访问。

/// §5 错误模型：`ProtocolError` 全变体 + `UnlockStep`/`CommandStep`。
pub mod error;

/// §4.10 原子编码/解码（token / tiny / 窄整数 / 短中长字节串 / UID）。
pub mod atom;

/// §4.10 UID 表常量。
pub mod uid;

/// 报文头 Make/Parse、状态列表单/双变体、方法状态字节、`token[i]` 遍历。
pub mod frame;

/// `identify_device` / `parse_level0` / `LockingFlags`（REQ-001/002）。
pub mod discovery;

/// `start_session_payload` / `validate_password_payload` / `session_ids_from_response`
/// （REQ-006/009）。
pub mod session;

/// FB / 4×`Set` / FC / FA 载荷构造（REQ-007）。
pub mod transaction;

/// `evaluate_unlock` / `ReEnumerationObservation` / `UnlockEvidence`（REQ-008）。
pub mod unlock;

/// 会话状态机 + 命令编排 + 中止路径（§3.3、REQ-011 占位错误）。
pub mod runner;

/// `Password(Zeroizing<Vec<u8>>)` 及其生命周期。
pub mod password;

// 对外门面：应用层（`magi-app`）按 `magi_protocol::<名字>` 取用这些项；其余构造细节按
// `magi_protocol::<module>::…` 访问（模块本身全部公开）。
pub use discovery::{
    discovery_cdb, identify_device, parse_level0, DeviceState, Discovery, FeatureDescriptor,
    LockingFlags, PID_LOCKED, PID_UNLOCKED, VENDOR_ID,
};
pub use error::{CommandStep, ProtocolError, UnlockStep};
pub use frame::{StatusListForm, TcgResponse};
pub use password::Password;
pub use runner::{
    classify_transport_error, delete_password, discover, run_unlock, run_validate_password,
    set_password, OperationContext, ProgressReporter, RunError, SessionState, UnlockSession,
};
pub use session::{SessionIds, StartSessionRequest, ValidateOutcome};
pub use transaction::{unlock_sets, SetCell, SetRow, UnlockSetOp};
pub use unlock::{evaluate_unlock, ReEnumerationObservation, UnlockEvidence};

#[cfg(test)]
mod tests {
    use crate::*;
    /// W01 冒烟测试：确认 crate 可编译且元数据可见（脚手架验收，非行为测试）。
    #[test]
    fn test_crate_builds() {
        assert_eq!(env!("CARGO_PKG_NAME"), "magi-protocol");
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
    }

    /// 门面导出可用：应用层按 `magi_protocol::<名字>` 取用（spec §3.1 的单向依赖）。
    #[test]
    fn test_facade_exports_are_reachable() {
        assert_eq!(
            identify_device(VENDOR_ID, PID_LOCKED),
            Some(DeviceState::Locked)
        );
        assert_eq!(UnlockStep::ALL.len(), 7);
        let pwd = Password::new(b"secret".to_vec());
        assert_eq!(pwd.expose(), b"secret");
        assert_eq!(
            set_password(b"x"),
            Err(ProtocolError::PasswordOperationUnspecified)
        );
    }
}
