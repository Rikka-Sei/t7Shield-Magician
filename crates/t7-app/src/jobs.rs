//! §4.13 线程模型：工作线程 + channel + `AppEvent` 投递。
//!
//! - 协议 I/O 一律在工作线程执行，UI 主线程只做事件消费（§6：主线程单帧阻塞 ≤ 100 ms）；
//! - 同设备同时最多 1 个在飞作业（复用 [`crate::controller::JobRegistry`]；命令队列上限 1，
//!   溢出即拒绝，不排队）；
//! - 事件经 `async_channel` 回到主线程的 `glib::MainContext`，再由主窗口注册的事件汇消费。
//!
//! 事件汇是线程局部的：它捕获的是 glib 控件（`Send` 不成立），只允许在主线程注册与调用。

use std::cell::RefCell;
use std::sync::LazyLock;
use std::thread::{self, JoinHandle};

use async_channel::Receiver;
use gtk4::glib;
use t7_protocol::UnlockEvidence;
use t7_transport::transport::TransportError;

use crate::controller::{DeviceId, JobRegistry};
use crate::presentation::{AppError, AppEvent};

/// 全局单飞注册表：每设备 1 个工作线程（§6）。
static REGISTRY: LazyLock<JobRegistry> = LazyLock::new(JobRegistry::new);

/// 主线程事件汇的条目类型（`Fn` 闭包持有 glib 控件，不要求 `Send`）。
type EventSink = Box<dyn Fn(AppEvent)>;

thread_local! {
    /// 主线程事件汇（由主窗口在启动时注册一次）。
    static EVENT_SINK: RefCell<Option<EventSink>> = const { RefCell::new(None) };
}

/// 注册主线程事件汇；只允许注册一次，重复注册不覆盖并返回 `false`。
pub fn set_event_sink(sink: impl Fn(AppEvent) + 'static) -> bool {
    EVENT_SINK.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_some() {
            return false;
        }
        *slot = Some(Box::new(sink));
        true
    })
}

/// 把事件交给主线程事件汇；未注册时没有消费端，直接丢弃（不改变协议行为）。
fn deliver(event: AppEvent) {
    EVENT_SINK.with(|cell| {
        if let Some(sink) = cell.borrow().as_ref() {
            sink(event);
        }
    });
}

/// 在工作线程执行 `job`：`job` 用给定的发射器上报 [`AppEvent`]，返回值为收尾证据。
///
/// 返回事件通道与工作线程句柄；调用方负责消费通道（[`spawn_device_job`] 走主线程消费）。
/// 同设备已有在飞作业 → `AppError::Busy`（§4.13），且不启动线程、不下发任何命令。
pub fn run_device_job<F>(
    dev: DeviceId,
    job: F,
) -> Result<(Receiver<AppEvent>, JoinHandle<()>), AppError>
where
    F: FnOnce(&dyn Fn(AppEvent)) -> Result<Option<UnlockEvidence>, AppError> + Send + 'static,
{
    let guard = REGISTRY.try_begin(&dev)?;
    let (sender, receiver) = async_channel::unbounded::<AppEvent>();
    let emitter_sender = sender.clone();
    let spawned = thread::Builder::new()
        .name(String::from("t7-device-job"))
        .spawn(move || {
            // 单飞占位随本次作业（含失败与取消路径）一起释放。
            let _guard = guard;
            let emit = |event: AppEvent| {
                // 通道无界且消费端在主线程：投递不阻塞工作线程。
                let _ = emitter_sender.send_blocking(event);
            };
            let terminal = match job(&emit) {
                Ok(evidence) => AppEvent::Finished { evidence },
                Err(error) => AppEvent::Failed { error },
            };
            let _ = emitter_sender.send_blocking(terminal);
        });
    match spawned {
        Ok(handle) => Ok((receiver, handle)),
        // 线程创建失败是平台调用失败（§5：`Platform { code }` 承载平台原始错误码）。
        Err(err) => Err(AppError::Transport(TransportError::Platform {
            code: err.raw_os_error().map(i64::from).unwrap_or(-1),
        })),
    }
}

/// §4.13：在工作线程执行 `job`，把 `AppEvent` 投递回主线程（事件汇见 [`set_event_sink`]）。
///
/// 同设备最多一个在飞任务；已有在飞任务时返回 `AppError::Busy`。
pub fn spawn_device_job<F>(dev: DeviceId, job: F) -> Result<(), AppError>
where
    F: FnOnce(&dyn Fn(AppEvent)) -> Result<Option<UnlockEvidence>, AppError> + Send + 'static,
{
    let (receiver, _handle) = run_device_job(dev, job)?;
    glib::MainContext::default().spawn_local(async move {
        while let Ok(event) = receiver.recv().await {
            deliver(event);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use t7_protocol::UnlockStep;

    /// 工作线程把进度与收尾事件按序投递；单飞约束下同设备的第二次作业立即得到 `Busy`。
    #[test]
    fn test_job_reports_progress_and_single_flight() {
        let dev = DeviceId::new("test-device-0");
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let (receiver, handle) = run_device_job(dev.clone(), move |emit| {
            emit(AppEvent::Progress {
                step: UnlockStep::StartTransaction,
            });
            // 保持作业在飞，直到测试放行：此时第二次触发必须被判忙。
            blocked.recv().expect("测试放行信号");
            Ok(None)
        })
        .expect("首次作业必须被受理");

        assert_eq!(
            receiver.recv_blocking(),
            Ok(AppEvent::Progress {
                step: UnlockStep::StartTransaction
            })
        );
        assert_eq!(
            run_device_job(dev.clone(), |_| Ok(None)).expect_err("同设备第二次作业必须被判忙"),
            AppError::Busy
        );

        release.send(()).expect("放行在飞作业");
        assert_eq!(
            receiver.recv_blocking(),
            Ok(AppEvent::Finished { evidence: None })
        );
        handle.join().expect("工作线程必须正常结束");

        // 作业结束后单飞槽位已释放，同设备可再次受理。
        let (receiver, handle) = run_device_job(dev, |_| Ok(None)).expect("释放后必须可再次受理");
        assert_eq!(
            receiver.recv_blocking(),
            Ok(AppEvent::Finished { evidence: None })
        );
        handle.join().expect("工作线程必须正常结束");
    }

    /// 作业返回错误 → 收尾事件为 `Failed`（错误分类原样传到主线程）。
    #[test]
    fn test_job_failure_is_delivered_as_failed_event() {
        let dev = DeviceId::new("test-device-1");
        let (receiver, handle) = run_device_job(dev, |_| {
            Err(AppError::Transport(TransportError::DeviceGone))
        })
        .expect("作业必须被受理");
        assert_eq!(
            receiver.recv_blocking(),
            Ok(AppEvent::Failed {
                error: AppError::Transport(TransportError::DeviceGone)
            })
        );
        handle.join().expect("工作线程必须正常结束");
    }
}
