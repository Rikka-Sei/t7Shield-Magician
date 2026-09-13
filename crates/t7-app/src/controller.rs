//! 入口启用规则、单飞约束与设备身份准入（纯逻辑，无 GTK 依赖；K6：无头可测）。
//!
//! 口径来源：
//! - §4.1：锁定态/解锁态按 PID 裁决，非目标 PID 不识别、不发任何命令；
//! - §4.11/§4.12：口令写入口（设置/修改/删除）受证据缺口约束，任何设备态下都不可用；
//! - §4.13/§6：同设备单飞（`AppError::Busy`）、口令被拒最多 3 次、每设备 1 个工作线程；
//! - §4.5/§6：macOS 只做只读描述符侦察，盘操作入口一律禁用。
//!
//! 本模块不内联任何可显示字符串：文案键在此与 `locales/*.yml` 之间以键名交接（AC-015）。

use std::collections::HashSet;
use std::fmt;
use std::sync::Mutex;

use t7_protocol::{identify_device, DeviceState, Discovery, Password, RunError};
use t7_transport::transport::{DeviceTarget, Transport, TransportError};

use crate::presentation::{AppError, CODE_TRANSPORT_UNAVAILABLE};

/// §4.12 主窗口的操作入口。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionId {
    /// 解锁（锁定态可用）。
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

/// §4.1 的设备身份：由 VID/PID 裁决；未识别设备不产生任何操作入口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceIdentity {
    /// 锁定态（PID `0x61fc`）。
    Locked,
    /// 解锁态（PID `0x61fb`）。
    Unlocked,
    /// 重枚举窗口内（PID 尚未稳定）。
    ReEnumerating,
    /// 非目标 PID：不识别为 T7 Shield。
    Unrecognized,
}

impl DeviceIdentity {
    /// 按 VID/PID 裁决身份（`identify_device` 是唯一判据来源）。
    pub fn from_ids(vid: u16, pid: u16) -> Self {
        match identify_device(vid, pid) {
            Some(DeviceState::Locked) => DeviceIdentity::Locked,
            Some(DeviceState::Unlocked) => DeviceIdentity::Unlocked,
            Some(DeviceState::ReEnumerating) => DeviceIdentity::ReEnumerating,
            None => DeviceIdentity::Unrecognized,
        }
    }

    /// 对应的设备态；未识别为 `None`（启用矩阵的输入）。
    pub fn state(self) -> Option<DeviceState> {
        match self {
            DeviceIdentity::Locked => Some(DeviceState::Locked),
            DeviceIdentity::Unlocked => Some(DeviceState::Unlocked),
            DeviceIdentity::ReEnumerating => Some(DeviceState::ReEnumerating),
            DeviceIdentity::Unrecognized => None,
        }
    }

    /// 设备卡片的状态文案键。
    pub fn status_key(self) -> &'static str {
        match self {
            DeviceIdentity::Locked => "status.locked",
            DeviceIdentity::Unlocked => "status.unlocked",
            DeviceIdentity::ReEnumerating => "status.reenumerating",
            DeviceIdentity::Unrecognized => "status.unrecognized",
        }
    }
}

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

/// 设备稳定标识（Linux 上是 `/dev/sgN`，macOS 上是 `VID:PID`）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceId(String);

impl DeviceId {
    /// 由字符串构造（调用方保证同一设备每次得到相同取值）。
    pub fn new(id: impl Into<String>) -> Self {
        DeviceId(id.into())
    }

    /// 原始标识。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// §6 单飞注册表：每设备同时最多 1 个在飞操作；命令队列上限 1（溢出即拒绝，不排队）。
#[derive(Debug, Default)]
pub struct JobRegistry {
    in_flight: Mutex<HashSet<DeviceId>>,
}

impl JobRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        JobRegistry {
            in_flight: Mutex::new(HashSet::new()),
        }
    }

    /// 尝试为 `dev` 占位：已有在飞操作 → `AppError::Busy`（§4.13）。
    pub fn try_begin(&self, dev: &DeviceId) -> Result<JobGuard<'_>, AppError> {
        let mut in_flight = self.in_flight.lock().unwrap_or_else(|err| err.into_inner());
        if in_flight.contains(dev) {
            return Err(AppError::Busy);
        }
        in_flight.insert(dev.clone());
        Ok(JobGuard {
            dev: dev.clone(),
            registry: self,
        })
    }

    /// 当前在飞设备数。
    pub fn in_flight(&self) -> usize {
        self.in_flight
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .len()
    }
}

/// 在飞作业守卫：`Drop` 释放单飞槽位（成功、失败与取消路径都必须释放）。
#[derive(Debug)]
pub struct JobGuard<'a> {
    dev: DeviceId,
    registry: &'a JobRegistry,
}

impl JobGuard<'_> {
    /// 本守卫对应的设备标识。
    pub fn device(&self) -> &DeviceId {
        &self.dev
    }
}

impl Drop for JobGuard<'_> {
    fn drop(&mut self) {
        self.registry
            .in_flight
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .remove(&self.dev);
    }
}

/// §6 应用同时受理设备上限。
pub const MAX_DEVICES: usize = 8;

/// 设备数量超限的文案键（§6）。
pub const DEVICES_EXCEEDED_KEY: &str = "limit.devices_exceeded";

/// 扫描结果裁剪到同时受理上限（§6：8 个）；超出部分返回提示键。
pub fn clamp_devices<T>(found: Vec<T>) -> (Vec<T>, Option<&'static str>) {
    if found.len() > MAX_DEVICES {
        (
            found.into_iter().take(MAX_DEVICES).collect(),
            Some(DEVICES_EXCEEDED_KEY),
        )
    } else {
        (found, None)
    }
}

/// 当前平台能否执行盘操作（§4.5/§6：macOS 只做只读描述符侦察）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformCapability {
    /// 有可用的 SCSI 命令通道（Linux `SG_IO`）。
    ScsiAvailable,
    /// 只有只读描述符侦察（macОS）：任何盘操作立即不可用。
    MacOsDescriptorOnly,
}

impl PlatformCapability {
    /// 本机编译期的平台能力判定（运行期不轮询、不重试：§6 降级行为）。
    pub const fn current() -> Self {
        #[cfg(target_os = "macos")]
        {
            PlatformCapability::MacOsDescriptorOnly
        }
        #[cfg(not(target_os = "macos"))]
        {
            PlatformCapability::ScsiAvailable
        }
    }
}

/// 平台限制提示：入口可用性、限制呈现码、限制文案键。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformNotice {
    /// 入口可用性（每个入口一项）。
    pub actions: Vec<(ActionId, bool)>,
    /// 平台限制对应的呈现码；无限制时为 `None`。
    pub code: Option<&'static str>,
    /// 平台说明文案键（始终存在）。
    pub message_key: &'static str,
}

/// 按平台能力裁决入口可用性与限制文案（§4.5/§6）。
///
/// 盘操作不可用的平台（macOS 只读描述符侦察）全入口禁用，并给出呈现码
/// [`CODE_TRANSPORT_UNAVAILABLE`]；可用平台交由设备态矩阵裁决。
pub fn platform_notice(capability: PlatformCapability) -> PlatformNotice {
    match capability {
        PlatformCapability::ScsiAvailable => PlatformNotice {
            actions: allowed_actions(Some(DeviceState::Locked)),
            code: None,
            message_key: "platform.linux_notice",
        },
        PlatformCapability::MacOsDescriptorOnly => PlatformNotice {
            actions: ActionId::ALL
                .into_iter()
                .map(|action| (action, false))
                .collect(),
            code: Some(CODE_TRANSPORT_UNAVAILABLE),
            message_key: "platform.macos_notice",
        },
    }
}

/// 作业起步阶段的错误：身份未识别，或首个协议交互失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationStartError {
    /// 未识别为 T7 Shield（§4.1）：不打开通道、不下发命令。
    DeviceNotRecognized,
    /// 首个协议交互失败（与 `AppError` 同源，可直接进入呈现码表）。
    App(AppError),
}

impl OperationStartError {
    /// 失败原因文案键（未识别设备用设备卡状态键）。
    pub fn reason_key(&self) -> &'static str {
        match self {
            OperationStartError::DeviceNotRecognized => "status.unrecognized",
            OperationStartError::App(err) => crate::presentation::reason_key(err),
        }
    }
}

impl From<TransportError> for OperationStartError {
    fn from(err: TransportError) -> Self {
        OperationStartError::App(AppError::Transport(err))
    }
}

impl From<RunError> for OperationStartError {
    fn from(err: RunError) -> Self {
        OperationStartError::App(err.into())
    }
}

/// 作业第一步（§4.1/§4.2）：先按身份裁决，再做 Level-0 Discovery。
///
/// 未识别身份立即返回，**不打开传输通道**，因此不会下发任何命令；识别成功时返回已打开的
/// 传输通道（供本作业后续的会话命令复用）与运行时解析出的 [`Discovery`]。
pub fn open_and_discover<T: Transport>(
    identity: DeviceIdentity,
    target: &DeviceTarget,
) -> Result<(T, Discovery), OperationStartError> {
    if identity == DeviceIdentity::Unrecognized {
        return Err(OperationStartError::DeviceNotRecognized);
    }
    let transport = T::open(target)?;
    let discovery = t7_protocol::discover(&transport)?;
    Ok((transport, discovery))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use t7_protocol::{PID_LOCKED, PID_UNLOCKED, VENDOR_ID};

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

    /// 锚点（§10）：非目标 PID 被拒绝且不下发命令。
    #[test]
    fn test_unknown_pid_is_rejected() {
        assert_eq!(
            identify_device(VENDOR_ID, PID_LOCKED),
            Some(DeviceState::Locked)
        );
        assert_eq!(
            DeviceIdentity::from_ids(VENDOR_ID, 0x61ff),
            DeviceIdentity::Unrecognized
        );
        assert_eq!(
            DeviceIdentity::from_ids(0x1234, PID_LOCKED),
            DeviceIdentity::Unrecognized
        );
        assert!(allowed_actions(None).iter().all(|(_, enabled)| !enabled));

        // 未识别设备：`open_and_discover` 立即返回，传输通道的 `open` 一次都没被调用。
        CountingTransport::reset();
        let target = DeviceTarget::LinuxSg("/dev/sg0".to_string());
        let err = open_and_discover::<CountingTransport>(DeviceIdentity::Unrecognized, &target)
            .expect_err("未识别设备必须被拒绝");
        assert_eq!(err, OperationStartError::DeviceNotRecognized);
        assert_eq!(CountingTransport::open_count(), 0);
        assert_eq!(err.reason_key(), "status.unrecognized");
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

    /// 锚点（§10）：单飞约束下的忙错误。
    #[test]
    fn test_duplicate_trigger_is_busy() {
        let registry = JobRegistry::new();
        let dev = DeviceId::new("/dev/sg0");
        let other = DeviceId::new("/dev/sg1");

        let guard = registry.try_begin(&dev).expect("首次占位必须成功");
        assert_eq!(
            registry
                .try_begin(&dev)
                .expect_err("同设备第二次占位必须失败"),
            AppError::Busy
        );
        assert_eq!(registry.in_flight(), 1);

        // 不同设备可并行。
        let other_guard = registry.try_begin(&other).expect("不同设备必须可占位");
        assert_eq!(registry.in_flight(), 2);
        drop(other_guard);
        assert_eq!(registry.in_flight(), 1);

        // 守卫释放后可再次占位。
        drop(guard);
        assert_eq!(registry.in_flight(), 0);
        assert!(registry.try_begin(&dev).is_ok());
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

    /// 平台能力裁决：macOS 只读侦察全入口禁用并给出 `TransportUnavailable`（§4.5/AC-004）。
    #[test]
    fn test_macos_channel_disables_actions() {
        let notice = platform_notice(PlatformCapability::MacOsDescriptorOnly);
        assert!(notice.actions.iter().all(|(_, enabled)| !enabled));
        assert_eq!(notice.code, Some(CODE_TRANSPORT_UNAVAILABLE));
        assert_eq!(notice.message_key, "platform.macos_notice");

        let linux = platform_notice(PlatformCapability::ScsiAvailable);
        assert_eq!(linux.code, None);
        assert!(linux
            .actions
            .iter()
            .any(|(action, enabled)| *action == ActionId::Unlock && *enabled));
    }

    /// 受理设备上限 8 个，超出时给出提示键（§6）。
    #[test]
    fn test_device_limit() {
        let (all, notice) = clamp_devices(vec![0u8; MAX_DEVICES]);
        assert_eq!(all.len(), MAX_DEVICES);
        assert_eq!(notice, None);

        let (kept, notice) = clamp_devices(vec![0u8; MAX_DEVICES + 3]);
        assert_eq!(kept.len(), MAX_DEVICES);
        assert_eq!(notice, Some(DEVICES_EXCEEDED_KEY));
    }

    /// 身份识别：两个目标 PID 各归其态，其它 PID 一律未识别（§4.1）。
    #[test]
    fn test_device_identity_from_ids() {
        assert_eq!(
            DeviceIdentity::from_ids(VENDOR_ID, PID_LOCKED),
            DeviceIdentity::Locked
        );
        assert_eq!(
            DeviceIdentity::from_ids(VENDOR_ID, PID_UNLOCKED),
            DeviceIdentity::Unlocked
        );
        assert_eq!(
            DeviceIdentity::from_ids(VENDOR_ID, 0x61ff),
            DeviceIdentity::Unrecognized
        );
        assert_eq!(DeviceIdentity::Locked.status_key(), "status.locked");
        assert_eq!(DeviceIdentity::Unrecognized.state(), None);
    }

    /// 计数型假传输：只统计 `open` 次数（未识别设备下 `execute` 不会到达）。
    #[derive(Debug)]
    struct CountingTransport;

    static OPEN_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    impl CountingTransport {
        fn reset() {
            OPEN_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);
        }

        fn open_count() -> usize {
            OPEN_COUNT.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Transport for CountingTransport {
        fn open(_target: &DeviceTarget) -> Result<Self, TransportError> {
            OPEN_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(CountingTransport)
        }

        fn execute(
            &self,
            _cdb: &t7_transport::transport::ScsiCdb,
            _dir: t7_transport::transport::Direction,
            _data: &mut [u8],
            _timeout: Duration,
        ) -> Result<usize, TransportError> {
            Err(TransportError::Unavailable)
        }
    }
}
