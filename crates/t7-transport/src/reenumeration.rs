//! 重枚举观察：轮询判据的纯逻辑与采样结构。
//!
//! 口径（§4.8、§6）：解锁序列收尾后设备自行以新 PID 重新枚举，主机只观察、不发任何触发命令
//! （D06/AC-007）。观察窗口 30 s、每 500 ms 轮询一次、最多 60 次；窗口耗尽即按 §4.8 判据给结论。
//! 本模块只负责「按窗口与间隔采样」，判据裁决归协议层（`evaluate_unlock`）。

use std::thread;
use std::time::{Duration, Instant};

use crate::transport::TransportError;

/// 重枚举观察窗口（§6：30 s）。
pub const REENUMERATION_WINDOW: Duration = Duration::from_secs(30);

/// 重枚举轮询间隔（§6：500 ms）。
pub const REENUMERATION_INTERVAL: Duration = Duration::from_millis(500);

/// 一次观察采样（协议层据此裁决解锁判据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReEnumerationSample {
    /// 当前 USB PID；设备节点不存在时为 `None`。
    pub pid: Option<u16>,
    /// 是否观察到真实分区表。
    pub partition_table_seen: bool,
    /// 已挂载的卷（挂载点路径）。
    pub mounted_volumes: Vec<String>,
    /// 设备节点是否存在（重枚举期间会短暂为 `false`，属正常现象）。
    pub device_present: bool,
}

/// 给定观察窗口与轮询间隔，计算采样次数上限：30 s / 500 ms → 60 次（§6）。
///
/// 采样次数由窗口与间隔共同决定，因此二者是唯一的权威取值来源，不再另设「最多 60 次」常量。
pub fn sample_budget(window: Duration, interval: Duration) -> usize {
    let interval_nanos = interval.as_nanos();
    if interval_nanos == 0 {
        return 1;
    }
    let budget = window.as_nanos().div_ceil(interval_nanos);
    usize::try_from(budget.max(1)).unwrap_or(usize::MAX)
}

/// 按窗口与间隔轮询 `probe`，返回按时间顺序的采样结果。
///
/// 每次采样的结果都原样保留在返回值中：
/// - [`TransportError::DeviceGone`] 是重枚举窗口内的正常现象（设备节点此刻不存在），轮询继续；
/// - 其余错误原样返回该采样并结束轮询（窗口内不可能恢复正常，继续等只是让用户多等 30 s）；
/// - 采样次数上限为 [`sample_budget`]，且窗口耗尽即停止。
pub fn poll_reenumeration<P>(
    mut probe: P,
    window: Duration,
    interval: Duration,
) -> Vec<Result<ReEnumerationSample, TransportError>>
where
    P: FnMut() -> Result<ReEnumerationSample, TransportError>,
{
    let budget = sample_budget(window, interval);
    let start = Instant::now();
    let mut samples = Vec::with_capacity(budget);

    for index in 0..budget {
        if index > 0 {
            thread::sleep(interval);
        }
        let sample = probe();
        let stops_polling = match &sample {
            Ok(_) | Err(TransportError::DeviceGone) => false,
            Err(_) => true,
        };
        samples.push(sample);
        if stops_polling || start.elapsed() >= window {
            break;
        }
    }

    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn unlocked_sample() -> ReEnumerationSample {
        ReEnumerationSample {
            pid: Some(0x61FB),
            partition_table_seen: true,
            mounted_volumes: vec!["/run/media/user/T7".to_string()],
            device_present: true,
        }
    }

    /// §6 的量化取值：30 s 窗口、500 ms 间隔 → 60 次采样。
    #[test]
    fn test_reenumeration_window_and_budget() {
        assert_eq!(REENUMERATION_WINDOW, Duration::from_secs(30));
        assert_eq!(REENUMERATION_INTERVAL, Duration::from_millis(500));
        assert_eq!(
            sample_budget(REENUMERATION_WINDOW, REENUMERATION_INTERVAL),
            60
        );
        assert_eq!(sample_budget(Duration::from_millis(10), Duration::ZERO), 1);
    }

    /// 锚点：`test_device_gone_during_reenumeration`（spec §10、§7「重枚举期间设备节点消失」）。
    ///
    /// 设备在窗口内短暂消失（`DeviceGone`）不是失败：采样原样保留、轮询继续，直到设备重新出现。
    #[test]
    fn test_device_gone_during_reenumeration() {
        let calls = Cell::new(0usize);
        let window = Duration::from_millis(200);
        let interval = Duration::from_millis(1);

        let samples = poll_reenumeration(
            || {
                let index = calls.get() + 1;
                calls.set(index);
                if index <= 2 {
                    Err(TransportError::DeviceGone)
                } else {
                    Ok(unlocked_sample())
                }
            },
            window,
            interval,
        );

        assert!(
            matches!(samples[0], Err(TransportError::DeviceGone)),
            "消失期采样必须原样保留，不得被吞掉或改写"
        );
        assert!(matches!(samples[1], Err(TransportError::DeviceGone)));
        assert!(
            samples.len() >= 3 && samples.last().unwrap().is_ok(),
            "DeviceGone 不得中止轮询：设备回来后必须继续采样"
        );
        assert!(samples.len() <= sample_budget(window, interval));
        assert!(calls.get() == samples.len(), "每次采样恰好调用一次 probe");
    }

    /// 非「设备消失」的错误原样返回并结束轮询（不吞错、不换成 DeviceGone）。
    #[test]
    fn test_other_error_stops_polling_as_is() {
        let calls = Cell::new(0usize);
        let samples = poll_reenumeration(
            || {
                let index = calls.get() + 1;
                calls.set(index);
                if index == 1 {
                    Ok(ReEnumerationSample {
                        pid: None,
                        partition_table_seen: false,
                        mounted_volumes: Vec::new(),
                        device_present: false,
                    })
                } else {
                    Err(TransportError::PermissionDenied)
                }
            },
            REENUMERATION_WINDOW,
            REENUMERATION_INTERVAL,
        );

        assert_eq!(samples.len(), 2);
        assert!(samples[0].is_ok());
        assert_eq!(samples[1], Err(TransportError::PermissionDenied));
        assert_eq!(calls.get(), 2, "错误后不再继续轮询");
    }
}
