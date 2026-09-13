//! §4.8 解锁成功判据与重枚举观察。
//!
//! 判据按固定优先级取第一个可观察到的证据：① 真实分区表（可挂载）② Locking flags
//! `0x1F` → `0x3B` ③ USB PID `0x61fc` → `0x61fb`。仅 PID 变化时结论不等于分区表出现
//! （AC-007），客户端不发送任何重枚举触发命令。

use crate::discovery::{PID_LOCKED, PID_UNLOCKED};

/// §4.2 实测：锁定态 Locking flags 原始字节。
pub const LOCKING_FLAGS_LOCKED: u8 = 0x1F;
/// §4.2 实测：解锁态 Locking flags 原始字节（bit2 = 0、bit5 = 1）。
pub const LOCKING_FLAGS_UNLOCKED: u8 = 0x3B;

/// 解锁成功证据，按强度从强到弱。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockEvidence {
    /// ① 宿主出现真实分区表（`mounted_volumes` 为已挂载卷，可能为空）。
    RealPartitionTable { mounted_volumes: Vec<String> },
    /// ② 重读 discovery 后 Locking flags 由锁定态变为解锁态。
    LockingFlags { before: u8, after: u8 },
    /// ③ USB PID 由 `0x61fc` 变为 `0x61fb`。
    PidChange { before: u16, after: u16 },
}

/// 重枚举观察窗口内的一次判定输入（§4.8 契约）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReEnumerationObservation {
    /// 是否观察到匹配设备的真实分区表（出现子分区）。
    pub partition_table_seen: bool,
    /// 已挂载卷（如 `/dev/sda1`）。
    pub mounted_volumes: Vec<String>,
    /// 解锁序列前的 Locking flags 原始字节。
    pub locking_flags_before: u8,
    /// 窗口内重读 discovery 得到的 Locking flags（未观察到为 `None`）。
    pub locking_flags_after: Option<u8>,
    /// 窗口内重新枚举到的 USB PID（未观察到为 `None`）。
    pub pid_after: Option<u16>,
}

/// 按固定优先级返回最强证据；窗口耗尽（三个判据都未观察到）返回 `None`。
pub fn evaluate_unlock(obs: &ReEnumerationObservation) -> Option<UnlockEvidence> {
    if obs.partition_table_seen {
        return Some(UnlockEvidence::RealPartitionTable {
            mounted_volumes: obs.mounted_volumes.clone(),
        });
    }
    if obs.locking_flags_after == Some(LOCKING_FLAGS_UNLOCKED)
        && obs.locking_flags_before == LOCKING_FLAGS_LOCKED
    {
        return Some(UnlockEvidence::LockingFlags {
            before: obs.locking_flags_before,
            after: LOCKING_FLAGS_UNLOCKED,
        });
    }
    if obs.pid_after == Some(PID_UNLOCKED) {
        return Some(UnlockEvidence::PidChange {
            before: PID_LOCKED,
            after: PID_UNLOCKED,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation() -> ReEnumerationObservation {
        ReEnumerationObservation {
            partition_table_seen: false,
            mounted_volumes: Vec::new(),
            locking_flags_before: LOCKING_FLAGS_LOCKED,
            locking_flags_after: None,
            pid_after: None,
        }
    }

    /// ① 真实分区表最强：即使同时有 flags 与 PID 变化也返回 `RealPartitionTable`。
    #[test]
    fn test_partition_table_is_the_strongest_evidence() {
        let obs = ReEnumerationObservation {
            partition_table_seen: true,
            mounted_volumes: vec!["/dev/sda1".to_string()],
            locking_flags_after: Some(LOCKING_FLAGS_UNLOCKED),
            pid_after: Some(PID_UNLOCKED),
            ..observation()
        };
        assert_eq!(
            evaluate_unlock(&obs),
            Some(UnlockEvidence::RealPartitionTable {
                mounted_volumes: vec!["/dev/sda1".to_string()],
            })
        );
    }

    /// 分区表出现但尚无挂载卷：仍按 ① 返回，且卷列表为空（不得降级成 PID 判据）。
    #[test]
    fn test_partition_table_without_volumes_still_counts() {
        let obs = ReEnumerationObservation {
            partition_table_seen: true,
            pid_after: Some(PID_UNLOCKED),
            ..observation()
        };
        assert_eq!(
            evaluate_unlock(&obs),
            Some(UnlockEvidence::RealPartitionTable {
                mounted_volumes: Vec::new(),
            })
        );
    }

    /// ② flags `0x1F` → `0x3B` 强于 ③ PID 变化。
    #[test]
    fn test_locking_flags_beats_pid_change() {
        let obs = ReEnumerationObservation {
            locking_flags_after: Some(LOCKING_FLAGS_UNLOCKED),
            pid_after: Some(PID_UNLOCKED),
            ..observation()
        };
        assert_eq!(
            evaluate_unlock(&obs),
            Some(UnlockEvidence::LockingFlags {
                before: LOCKING_FLAGS_LOCKED,
                after: LOCKING_FLAGS_UNLOCKED,
            })
        );
    }

    /// flags 未变（或未观察到）时不得用 flags 判据；只有 `after` 变化才成立。
    #[test]
    fn test_locking_flags_requires_before_and_after() {
        let unchanged = ReEnumerationObservation {
            locking_flags_after: Some(LOCKING_FLAGS_LOCKED),
            ..observation()
        };
        assert_eq!(evaluate_unlock(&unchanged), None);

        let different_before = ReEnumerationObservation {
            locking_flags_before: 0x00,
            locking_flags_after: Some(LOCKING_FLAGS_UNLOCKED),
            ..observation()
        };
        assert_eq!(evaluate_unlock(&different_before), None);
    }

    /// ③ 只有 PID 变化 → `PidChange`（UI 文案区分于「已解锁并挂载」）。
    #[test]
    fn test_pid_change_only() {
        let obs = ReEnumerationObservation {
            pid_after: Some(PID_UNLOCKED),
            ..observation()
        };
        assert_eq!(
            evaluate_unlock(&obs),
            Some(UnlockEvidence::PidChange {
                before: PID_LOCKED,
                after: PID_UNLOCKED,
            })
        );
    }

    /// 三个判据都未观察到 → `None`（窗口耗尽，呈现「未观察到重枚举」）。
    #[test]
    fn test_window_exhausted_without_evidence() {
        assert_eq!(evaluate_unlock(&observation()), None);

        let still_locked = ReEnumerationObservation {
            locking_flags_after: Some(LOCKING_FLAGS_LOCKED),
            pid_after: Some(PID_LOCKED),
            ..observation()
        };
        assert_eq!(evaluate_unlock(&still_locked), None);
    }
}
