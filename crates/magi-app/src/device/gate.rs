//! 入口启用规则与口令输入校验（纯逻辑，无 GTK 依赖；K6：无头可测）。
//!
//! 口径来源：
//! - §4.11/§4.12：口令写入口（设置/修改/删除）受证据缺口约束，任何设备态下都不可用；
//! - §6：口令被拒最多 3 次，超过后本次会话禁用「解锁」入口；
//!
//! 本模块不内联任何可显示字符串：文案键在此与 `locales/*.yml` 之间以键名交接（AC-015）。

use magi_protocol::{DeviceState, Password};

use crate::presentation::AppError;

/// §4.12 主窗口的操作入口。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ActionId {
    /// 解锁（锁定态可用）。
    #[default]
    Unlock,
    /// 校验口令（锁定态可用）。
    ValidatePassword,
    /// 设置口令（证据缺口：不可用）。
    SetPassword,
    /// 修改口令（证据缺口：不可用）。
    ChangePassword,
    /// 删除口令（证据缺口：不可用）。
    DeletePassword,
}

impl ActionId {
    /// 入口全集与固定顺序（UI 按此顺序装配按钮）。
    pub const ALL: [ActionId; 5] = [
        ActionId::Unlock,
        ActionId::ValidatePassword,
        ActionId::SetPassword,
        ActionId::ChangePassword,
        ActionId::DeletePassword,
    ];

    /// 口令写入口（§4.11：字节级序列未证实，受证据缺口约束）。
    pub fn is_password_write(self) -> bool {
        matches!(
            self,
            ActionId::SetPassword | ActionId::ChangePassword | ActionId::DeletePassword
        )
    }

    /// 入口标签的 i18n 键。
    pub fn label_key(self) -> &'static str {
        match self {
            ActionId::Unlock => "action.unlock",
            ActionId::ValidatePassword => "action.validate_password",
            ActionId::SetPassword => "action.set_password",
            ActionId::ChangePassword => "action.change_password",
            ActionId::DeletePassword => "action.delete_password",
        }
    }
}

/// 禁用理由的 i18n 键（`None` = 该入口在当前设备态下可用）。
pub const REASON_EVIDENCE_GAP: &str = "reason.evidence_gap";
/// 禁用理由：重枚举窗口内等待（§3.3）。
pub const REASON_REENUMERATING: &str = "reason.reenumerating";
/// 禁用理由：设备非锁定态（§3.3）。
pub const REASON_NOT_LOCKED: &str = "reason.not_locked";
/// 禁用理由：未发现 T7 Shield（§4.1）。
pub const REASON_UNRECOGNIZED: &str = "reason.unrecognized";
/// 禁用理由：口令重试预算耗尽（§6：3 次）。
pub const REASON_RETRIES_EXHAUSTED: &str = "reason.retries_exhausted";

/// 入口可用性矩阵（§4.12：锁定态解锁/校验可用、写口令入口禁用；其余设备态全禁用）。
pub fn allowed_actions(state: Option<DeviceState>) -> Vec<(ActionId, bool)> {
    ActionId::ALL
        .into_iter()
        .map(|action| (action, is_action_allowed(action, state)))
        .collect()
}

/// 单个入口的可用性：只有锁定态下的「解锁」与「校验口令」可用。
pub fn is_action_allowed(action: ActionId, state: Option<DeviceState>) -> bool {
    if action.is_password_write() {
        return false;
    }
    matches!(
        (action, state),
        (
            ActionId::Unlock | ActionId::ValidatePassword,
            Some(DeviceState::Locked)
        )
    )
}

/// 入口被禁用的理由键（可用时为 `None`）。
pub fn disabled_reason_key(action: ActionId, state: Option<DeviceState>) -> Option<&'static str> {
    if action.is_password_write() {
        return Some(REASON_EVIDENCE_GAP);
    }
    match state {
        None => Some(REASON_UNRECOGNIZED),
        Some(DeviceState::ReEnumerating) => Some(REASON_REENUMERATING),
        Some(DeviceState::Unlocked) => Some(REASON_NOT_LOCKED),
        Some(DeviceState::Locked) => None,
    }
}

/// 口令被拒的最大次数（§6：超过后本次会话禁用「解锁」入口，直到重新打开对话框）。
pub const MAX_PASSWORD_REJECTIONS: u32 = 3;

/// 口令被拒计数（§6）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PasswordAttempts {
    rejected: u32,
}

impl PasswordAttempts {
    /// 新会话的计数（0 次被拒）。
    pub const fn new() -> Self {
        PasswordAttempts { rejected: 0 }
    }

    /// 已被拒次数。
    pub const fn rejected(self) -> u32 {
        self.rejected
    }

    /// 记录一次「口令被拒」。
    pub fn record_rejection(&mut self) {
        self.rejected = self.rejected.saturating_add(1);
    }

    /// 重试预算是否耗尽（耗尽后禁用「解锁」入口）。
    pub const fn exhausted(self) -> bool {
        self.rejected >= MAX_PASSWORD_REJECTIONS
    }

    /// 用户重新打开对话框 → 本会话计数归零（§6）。
    pub fn reset(&mut self) {
        self.rejected = 0;
    }
}

/// 入口闸门：把「设备态启用矩阵」与「口令重试预算」合成 UI 的最终可用性。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UnlockGate {
    attempts: PasswordAttempts,
}

impl UnlockGate {
    /// 新闸门（计数 0）。
    pub const fn new() -> Self {
        UnlockGate {
            attempts: PasswordAttempts::new(),
        }
    }

    /// 当前重试计数。
    pub const fn attempts(self) -> PasswordAttempts {
        self.attempts
    }

    /// 记录一次「口令被拒」（由 `AppError::PasswordRejected` 触发）。
    pub fn record_rejection(&mut self) {
        self.attempts.record_rejection();
    }

    /// 重新打开对话框：重试预算复位（§6）。
    pub fn open_dialog(&mut self) {
        self.attempts.reset();
    }

    /// 入口是否可用（设备态矩阵 + 重试预算）。
    pub fn allows(self, action: ActionId, state: Option<DeviceState>) -> bool {
        if self.attempts.exhausted() {
            return false;
        }
        is_action_allowed(action, state)
    }

    /// 入口被禁用的理由键（可用时为 `None`）。
    pub fn disabled_reason_key(
        self,
        action: ActionId,
        state: Option<DeviceState>,
    ) -> Option<&'static str> {
        if self.attempts.exhausted() {
            return Some(REASON_RETRIES_EXHAUSTED);
        }
        disabled_reason_key(action, state)
    }
}

/// §4.12：空口令（或全空白）拒绝，不构造任何报文；非空输入按 UTF-8 字节接管为
/// [`Password`]（Zeroizing 缓冲，不进入任何 `String` 持有的长生命周期结构）。
pub fn validate_password_input(input: &str) -> Result<Password, AppError> {
    if input.trim().is_empty() {
        return Err(AppError::EmptyPassword);
    }
    Ok(Password::new(input.as_bytes().to_vec()))
}


#[cfg(test)]
mod tests {
    use super::*;

    /// 锚点（§10）：设备态驱动的入口启用与禁用。
    #[test]
    fn test_locked_device_actions_disabled() {
        // 锁定态：解锁 + 校验口令可用，三个写口令入口因证据缺口禁用。
        let locked = allowed_actions(Some(DeviceState::Locked));
        assert_eq!(
            locked,
            vec![
                (ActionId::Unlock, true),
                (ActionId::ValidatePassword, true),
                (ActionId::SetPassword, false),
                (ActionId::ChangePassword, false),
                (ActionId::DeletePassword, false),
            ]
        );
        assert_eq!(
            disabled_reason_key(ActionId::SetPassword, Some(DeviceState::Locked)),
            Some(REASON_EVIDENCE_GAP)
        );
        assert_eq!(
            disabled_reason_key(ActionId::Unlock, Some(DeviceState::Locked)),
            None
        );

        // 其余设备态：全部禁用。
        for state in [
            Some(DeviceState::Unlocked),
            Some(DeviceState::ReEnumerating),
            None,
        ] {
            for (action, enabled) in allowed_actions(state) {
                assert!(!enabled, "{state:?} 下 {action:?} 不应可用");
            }
        }
        assert_eq!(
            disabled_reason_key(ActionId::Unlock, Some(DeviceState::Unlocked)),
            Some(REASON_NOT_LOCKED)
        );
        assert_eq!(
            disabled_reason_key(ActionId::Unlock, Some(DeviceState::ReEnumerating)),
            Some(REASON_REENUMERATING)
        );
        assert_eq!(
            disabled_reason_key(ActionId::Unlock, None),
            Some(REASON_UNRECOGNIZED)
        );

        // 闸门叠加：重试预算耗尽后连锁定态入口也禁用，重开对话框后恢复。
        let mut gate = UnlockGate::new();
        assert!(gate.allows(ActionId::Unlock, Some(DeviceState::Locked)));
        for _ in 0..MAX_PASSWORD_REJECTIONS {
            gate.record_rejection();
        }
        assert_eq!(gate.attempts().rejected(), MAX_PASSWORD_REJECTIONS);
        assert!(!gate.allows(ActionId::Unlock, Some(DeviceState::Locked)));
        assert_eq!(
            gate.disabled_reason_key(ActionId::Unlock, Some(DeviceState::Locked)),
            Some(REASON_RETRIES_EXHAUSTED)
        );
        gate.open_dialog();
        assert!(gate.allows(ActionId::Unlock, Some(DeviceState::Locked)));
    }

    /// 锚点（§10）：空口令被拒绝且不构造报文。
    #[test]
    fn test_empty_password_rejected() {
        assert_eq!(
            validate_password_input("").expect_err("空口令必须被拒绝"),
            AppError::EmptyPassword
        );
        assert_eq!(
            validate_password_input("   ").expect_err("全空白口令必须被拒绝"),
            AppError::EmptyPassword
        );
        let mut pwd = validate_password_input("hunter2").expect("非空口令必须被接受");
        assert_eq!(pwd.expose(), b"hunter2");
        pwd.zeroize_now();
        assert!(pwd.expose().is_empty());
        // 非 ASCII 口令按 UTF-8 字节接管（长度 = 字节数）。
        let pwd = validate_password_input("口令").expect("非空口令必须被接受");
        assert_eq!(pwd.expose().len(), "口令".len());
    }

    /// 口令重试预算：每次被拒递减，超过 3 次后不再允许提交（§6）。
    #[test]
    fn test_password_retry_budget() {
        let mut attempts = PasswordAttempts::new();
        assert_eq!(attempts.rejected(), 0);
        assert!(!attempts.exhausted());
        for expected in 1..=MAX_PASSWORD_REJECTIONS {
            attempts.record_rejection();
            assert_eq!(attempts.rejected(), expected);
        }
        assert!(attempts.exhausted());
        attempts.reset();
        assert!(!attempts.exhausted());
    }

}
