//! 重枚举观察：`ReEnumerationSample` → `ReEnumerationObservation` 的纯映射与轮询驱动。
//!
//! 采样与窗口口径归 `magi-transport`（§6：30 s 窗口、500 ms 间隔、最多 60 次）；判据裁决归
//! `magi-protocol::evaluate_unlock`（§4.8）。本模块只做两者之间的搬运，不引入新的判据：
//! 分区表/挂载取窗口内任一采样，PID 取最后一个成功采样的现值。

use magi_protocol::{evaluate_unlock, ReEnumerationObservation, UnlockEvidence};
use magi_transport::reenumeration::{
    poll_reenumeration, ReEnumerationSample, REENUMERATION_INTERVAL, REENUMERATION_WINDOW,
};
use magi_transport::transport::TransportError;

/// 采样序列 → 判据输入（§4.8）。
///
/// - `partition_table_seen`：窗口内任一采样观察到真实分区表；
/// - `mounted_volumes`：窗口内出现过的挂载点并集（按首次出现顺序）；
/// - `pid_after`：最后一个采样的 PID（设备节点不存在时为 `None`）；
/// - `locking_flags_before` / `locking_flags_after`：由调用方给出（解锁前值 + 窗口内重读值）。
pub fn to_observation(
    samples: &[ReEnumerationSample],
    flags_before: u8,
    flags_after: Option<u8>,
) -> ReEnumerationObservation {
    let partition_table_seen = samples.iter().any(|sample| sample.partition_table_seen);
    let mut mounted_volumes: Vec<String> = Vec::new();
    for sample in samples {
        for volume in &sample.mounted_volumes {
            if !mounted_volumes.contains(volume) {
                mounted_volumes.push(volume.clone());
            }
        }
    }
    ReEnumerationObservation {
        partition_table_seen,
        mounted_volumes,
        locking_flags_before: flags_before,
        locking_flags_after: flags_after,
        pid_after: samples.last().and_then(|sample| sample.pid),
    }
}

/// 在重枚举窗口内轮询并裁决（§4.8/§6）：返回采样序列与解锁判据（窗口耗尽为 `None`）。
///
/// 判据固定优先级「真实分区表 > Locking flags 0x1F→0x3B > PID 变化」由
/// `magi_protocol::evaluate_unlock` 裁决；`read_flags_after` 在窗口结束后重读一次
/// Locking flags（判据②，失败返回 `None`）；本函数不发送任何重枚举触发命令（AC-007）。
pub fn observe_and_evaluate<P, F>(
    probe: P,
    flags_before: u8,
    read_flags_after: F,
) -> (
    Vec<Result<ReEnumerationSample, TransportError>>,
    Option<UnlockEvidence>,
)
where
    P: FnMut() -> Result<ReEnumerationSample, TransportError>,
    F: FnOnce() -> Option<u8>,
{
    let samples = poll_reenumeration(probe, REENUMERATION_WINDOW, REENUMERATION_INTERVAL);
    let observed: Vec<ReEnumerationSample> = samples
        .iter()
        .filter_map(|sample| sample.as_ref().ok().cloned())
        .collect();
    let flags_after = read_flags_after();
    let evidence = evaluate_unlock(&to_observation(&observed, flags_before, flags_after));
    (samples, evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use magi_protocol::unlock::{LOCKING_FLAGS_LOCKED, LOCKING_FLAGS_UNLOCKED};
    use magi_protocol::{PID_LOCKED, PID_UNLOCKED};

    fn sample(
        pid: Option<u16>,
        partition_table_seen: bool,
        mounted_volumes: &[&str],
        device_present: bool,
    ) -> ReEnumerationSample {
        ReEnumerationSample {
            pid,
            partition_table_seen,
            mounted_volumes: mounted_volumes.iter().map(|v| v.to_string()).collect(),
            device_present,
        }
    }

    /// 分区表与挂载卷取窗口内并集，PID 取最后一个采样。
    #[test]
    fn test_observation_maps_samples() {
        let samples = vec![
            sample(None, false, &[], false),
            sample(Some(PID_LOCKED), false, &[], true),
            sample(Some(PID_UNLOCKED), true, &["/media/t7"], true),
            sample(Some(PID_UNLOCKED), true, &["/media/t7", "/media/t7b"], true),
        ];
        let obs = to_observation(&samples, LOCKING_FLAGS_LOCKED, Some(LOCKING_FLAGS_UNLOCKED));
        assert!(obs.partition_table_seen);
        assert_eq!(obs.mounted_volumes, vec!["/media/t7", "/media/t7b"]);
        assert_eq!(obs.pid_after, Some(PID_UNLOCKED));
        assert_eq!(
            evaluate_unlock(&obs),
            Some(UnlockEvidence::RealPartitionTable {
                mounted_volumes: vec!["/media/t7".to_string(), "/media/t7b".to_string()],
            })
        );
    }

    /// 空采样序列：三个判据都未观察到 → `None`（窗口耗尽）。
    #[test]
    fn test_empty_samples_yield_no_evidence() {
        let obs = to_observation(&[], LOCKING_FLAGS_LOCKED, None);
        assert!(!obs.partition_table_seen);
        assert!(obs.mounted_volumes.is_empty());
        assert_eq!(obs.pid_after, None);
        assert_eq!(evaluate_unlock(&obs), None);
    }

    /// 只有 PID 变化：判据降级为 `PidChange`，不等于分区表出现（§4.8/AC-007）。
    #[test]
    fn test_pid_change_is_weaker_evidence() {
        let samples = vec![sample(Some(PID_UNLOCKED), false, &[], true)];
        let obs = to_observation(&samples, LOCKING_FLAGS_LOCKED, None);
        assert_eq!(
            evaluate_unlock(&obs),
            Some(UnlockEvidence::PidChange {
                before: PID_LOCKED,
                after: PID_UNLOCKED,
            })
        );
    }

    /// 轮询驱动：`DeviceGone` 是窗口内正常现象，最终采样可见（§7）。
    #[test]
    fn test_poll_driver_tolerates_device_gone() {
        use std::cell::Cell;

        let step = Cell::new(0usize);
        let mut probe = || {
            let current = step.get();
            step.set(current + 1);
            match current {
                0 | 1 => Err(TransportError::DeviceGone),
                _ => Ok(sample(Some(PID_UNLOCKED), true, &["/media/t7"], true)),
            }
        };
        // 小尺度等价窗口（1 ms 间隔 × 5 = 5 ms 窗口，判据与 30 s/500 ms 同源），避免真等 30 s。
        let interval = Duration::from_millis(1);
        let samples = poll_reenumeration(&mut probe, interval * 5, interval);
        assert!(samples.len() >= 2, "轮询应至少覆盖两次采样");
        assert!(matches!(samples[0], Err(TransportError::DeviceGone)));
        assert!(samples.last().expect("非空").is_ok());
        let observed: Vec<ReEnumerationSample> = samples
            .iter()
            .filter_map(|sample| sample.as_ref().ok().cloned())
            .collect();
        let evidence = evaluate_unlock(&to_observation(
            &observed,
            LOCKING_FLAGS_LOCKED,
            Some(LOCKING_FLAGS_UNLOCKED),
        ));
        assert!(matches!(
            evidence,
            Some(UnlockEvidence::RealPartitionTable { .. })
        ));
    }
}
