//! 设备身份准入与周期重扫的呈现裁决（纯逻辑，无 GTK 依赖；K6：无头可测）。
//!
//! 口径来源：
//! - §4.1：锁定态/解锁态按 PID 裁决，非目标 PID 不识别、不发任何命令；
//! - §3.3：拔出后第二轮才切空态（防抖）；插入不防抖，命中当轮即呈现；
//! - §6：应用同时受理设备上限 8 个。

use magi_protocol::{identify_device, DeviceState};

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

/// 周期重扫切换「未发现」空态所需的连续未命中轮数（§3.3 空态防抖一轮：拔出后第二轮
/// 才切空态，避免瞬时枚举抖动误报；插入方向不防抖，命中当轮即呈现）。
pub const RESCAN_MISS_ROUNDS: u32 = 2;

/// 周期重扫一轮的呈现动作（§4.1/§3.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RescanAction {
    /// 呈现命中设备：更新设备卡片、徽章与入口（含锁定↔解锁的 PID 变化）。
    ShowHit,
    /// 呈现「未发现 T7 Shield」空态。
    ShowEmpty,
    /// 维持现状：防抖等待、身份未变化或作业在飞（§6：不打断进行中的操作呈现）。
    Keep,
}

/// 周期重扫的呈现状态机（纯逻辑，无 GTK 依赖；K6：无头可测）。
///
/// 每轮把扫描命中与在飞标志喂给 [`RescanState::observe`]，得到下一状态与呈现动作：
/// - 插入 → 当轮呈现命中态（REQ-001：设备接入后自动识别）；
/// - 拔出 → 连续 [`RESCAN_MISS_ROUNDS`] 轮未见才切空态；
/// - 作业在飞 → 只更新在位缓存 `presence`，不改呈现、不计防抖（作业结束后下一轮自然收敛）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RescanState {
    /// 当前呈现中的身份（`None` = 未发现 T7 Shield）。
    presented: Option<DeviceIdentity>,
    /// 在位缓存：最近一轮扫描命中的身份（含在飞轮次；在飞轮次的呈现动作被推迟）。
    presence: Option<DeviceIdentity>,
    /// 「作业不在飞」的连续未命中轮数。
    misses: u32,
}

impl RescanState {
    /// 初始状态：未呈现任何设备。
    pub fn new() -> Self {
        RescanState::default()
    }

    /// 当前呈现中的身份（`None` = 未发现 T7 Shield）。
    pub fn presented(&self) -> Option<DeviceIdentity> {
        self.presented
    }

    /// 在位缓存（最近一轮扫描命中，含在飞轮次）。
    pub fn presence(&self) -> Option<DeviceIdentity> {
        self.presence
    }

    /// 消费一轮扫描结果，裁决下一状态与呈现动作。
    ///
    /// `scan` 是本轮命中的身份（`None` = 未命中或扫描失败）；`in_flight` 表示当前有
    /// 设备作业在飞（§4.13 单飞）。
    pub fn observe(
        mut self,
        scan: Option<DeviceIdentity>,
        in_flight: bool,
    ) -> (RescanState, RescanAction) {
        self.presence = scan;
        if in_flight {
            // §6：在飞期间呈现被用户操作占据 —— 只记在位缓存，防抖与呈现整体推迟。
            return (self, RescanAction::Keep);
        }
        match scan {
            Some(identity) => {
                let changed = self.presented != Some(identity);
                self.presented = Some(identity);
                self.misses = 0;
                if changed {
                    (self, RescanAction::ShowHit)
                } else {
                    (self, RescanAction::Keep)
                }
            }
            None => {
                self.misses = self.misses.saturating_add(1);
                if self.presented.is_some() && self.misses >= RESCAN_MISS_ROUNDS {
                    self.presented = None;
                    (self, RescanAction::ShowEmpty)
                } else {
                    (self, RescanAction::Keep)
                }
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use magi_protocol::{PID_LOCKED, PID_UNLOCKED, VENDOR_ID};

    use super::*;

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

    /// 周期重扫：插入 → 当轮呈现命中态（REQ-001：设备接入后自动识别）。
    #[test]
    fn test_rescan_insert_presents_hit_immediately() {
        let (state, action) = RescanState::new().observe(Some(DeviceIdentity::Locked), false);
        assert_eq!(action, RescanAction::ShowHit);
        assert_eq!(state.presented(), Some(DeviceIdentity::Locked));
        // 同一身份重复扫描：维持现状，不重绘（避免无谓的卡片抖动）。
        let (state, action) = state.observe(Some(DeviceIdentity::Locked), false);
        assert_eq!(action, RescanAction::Keep);
        assert_eq!(state.presented(), Some(DeviceIdentity::Locked));
    }

    /// 周期重扫：拔出 → 防抖一轮（连续两轮未见才切空态）；再次插入 → 恢复命中态（§3.3）。
    #[test]
    fn test_rescan_removal_debounces_before_empty() {
        let (state, _) = RescanState::new().observe(Some(DeviceIdentity::Locked), false);
        // 第一轮未见：防抖等待，命中态保持。
        let (state, action) = state.observe(None, false);
        assert_eq!(action, RescanAction::Keep);
        assert_eq!(state.presented(), Some(DeviceIdentity::Locked));
        // 第二轮仍未命中：切换「未发现」空态。
        let (state, action) = state.observe(None, false);
        assert_eq!(action, RescanAction::ShowEmpty);
        assert_eq!(state.presented(), None);
        // 再次插入：恢复命中态。
        let (state, action) = state.observe(Some(DeviceIdentity::Locked), false);
        assert_eq!(action, RescanAction::ShowHit);
        assert_eq!(state.presented(), Some(DeviceIdentity::Locked));
        // 再次拔出：防抖计数已归零，仍需完整两轮（第一轮 Keep，第二轮 ShowEmpty）。
        let (state, action) = state.observe(None, false);
        assert_eq!(action, RescanAction::Keep);
        assert_eq!(state.presented(), Some(DeviceIdentity::Locked));
        let (state, action) = state.observe(None, false);
        assert_eq!(action, RescanAction::ShowEmpty);
        assert_eq!(state.presented(), None);
        // 空态已呈现后持续未发现：不再重复切换。
        let (state, action) = state.observe(None, false);
        assert_eq!(action, RescanAction::Keep);
        assert_eq!(state.presented(), None);
        let (_, action) = state.observe(None, false);
        assert_eq!(action, RescanAction::Keep);
    }

    /// 周期重扫：作业在飞 → 只更新在位缓存，不改呈现、不计防抖；作业结束后下一轮收敛。
    #[test]
    fn test_rescan_in_flight_updates_presence_cache_only() {
        let (state, _) = RescanState::new().observe(Some(DeviceIdentity::Locked), false);
        // 在飞轮次命中已变为解锁态：呈现保持锁定态，缓存记录解锁态。
        let (state, action) = state.observe(Some(DeviceIdentity::Unlocked), true);
        assert_eq!(action, RescanAction::Keep);
        assert_eq!(state.presented(), Some(DeviceIdentity::Locked));
        assert_eq!(state.presence(), Some(DeviceIdentity::Unlocked));
        // 在飞期间的未命中同样不计防抖（呈现动作整体推迟到作业结束后）。
        let (state, action) = state.observe(None, true);
        assert_eq!(action, RescanAction::Keep);
        assert_eq!(state.presented(), Some(DeviceIdentity::Locked));
        // 作业结束：下一轮自然收敛到解锁态。
        let (state, action) = state.observe(Some(DeviceIdentity::Unlocked), false);
        assert_eq!(action, RescanAction::ShowHit);
        assert_eq!(state.presented(), Some(DeviceIdentity::Unlocked));
    }

    /// 周期重扫：锁定 → 解锁（PID 0x61fc → 0x61fb，解锁重枚举后）→ 徽章随身份更新。
    #[test]
    fn test_rescan_identity_change_updates_badge() {
        let (state, _) = RescanState::new().observe(Some(DeviceIdentity::Locked), false);
        // PID 变化被捕获并要求重绘（徽章换色）。
        let (state, action) = state.observe(Some(DeviceIdentity::Unlocked), false);
        assert_eq!(action, RescanAction::ShowHit);
        assert_eq!(state.presented(), Some(DeviceIdentity::Unlocked));
        // 重枚举过渡态同样被捕获。
        let (state, action) = state.observe(Some(DeviceIdentity::ReEnumerating), false);
        assert_eq!(action, RescanAction::ShowHit);
        assert_eq!(state.presented(), Some(DeviceIdentity::ReEnumerating));
        // 过渡态收敛回解锁态。
        let (state, action) = state.observe(Some(DeviceIdentity::Unlocked), false);
        assert_eq!(action, RescanAction::ShowHit);
        assert_eq!(state.presented(), Some(DeviceIdentity::Unlocked));
    }

}
