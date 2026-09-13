//! t7-protocol：T7 Shield 协议层（纯协议逻辑，零 UI 依赖）。
//!
//! 依赖方向：`t7-app` → `t7-protocol` → `t7-transport`（反向禁止）。本 crate 负责 Level-0
//! Discovery 解析、TCG 帧构造与响应解析、会话状态机与解锁判据。所有协议行为以
//! `docs/specs/t7-magician/spec.md` 为唯一权威；本层不做 I/O，也不定义 CDB 字节
//! （CDB 归 `t7-transport::cdb`）。
//!
//! 安全约束：口令缓冲归本 crate 所有（`Password(Zeroizing<Vec<u8>>)`），构造完成后立即
//! zeroize 并在 `Drop` 二次清零；口令内容与长度都不写日志。
//!
//! W01 仅建立模块骨架：各模块目前只有职责说明，行为由后续任务填充。

/// §5 错误模型：`ProtocolError` 全变体 + `UnlockStep`。
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

#[cfg(test)]
mod tests {
    /// W01 冒烟测试：确认 crate 可编译且元数据可见（脚手架验收，非行为测试）。
    #[test]
    fn test_crate_builds() {
        assert_eq!(env!("CARGO_PKG_NAME"), "t7-protocol");
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
    }
}
