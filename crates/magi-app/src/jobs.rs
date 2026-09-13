//! §4.13 线程模型：工作线程 + channel + `AppEvent` 投递。
//!
//! - 协议 I/O 一律在工作线程执行，UI 主线程只做事件消费（§6：主线程单帧阻塞 ≤ 100 ms）；
//! - 同设备同时最多 1 个在飞作业（复用 [`crate::controller::JobRegistry`]；命令队列上限 1，
//!   溢出即拒绝，不排队）；
//! - 事件经 `async_channel` 回到主线程的 `glib::MainContext`，再由主窗口注册的事件汇消费。
//!
//! 事件汇是线程局部的：它捕获的是 glib 控件（`Send` 不成立），只允许在主线程注册与调用。

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use async_channel::Receiver;
use gtk4::glib;
use magi_protocol::{
    run_unlock, run_validate_password, DeviceState, Password, ProgressReporter, UnlockEvidence,
    UnlockStep, VENDOR_ID,
};
use magi_transport::transport::{DeviceTarget, Transport, TransportError};
use magi_transport::usb_descriptor::UsbDescriptorSummary;
use rust_i18n::t;

use crate::controller::{self, DeviceId, DeviceIdentity, JobRegistry};
use crate::presentation::{AppError, AppEvent};

/// 全局单飞注册表：每设备 1 个工作线程（§6）。
static REGISTRY: LazyLock<JobRegistry> = LazyLock::new(JobRegistry::new);
/// 是否有任一设备作业在飞（§4.13：周期重扫在飞轮次只记在位缓存，不改呈现）。
pub fn any_job_in_flight() -> bool {
    REGISTRY.in_flight() > 0
}

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

/// 平台命令通道（§4.5）：Linux 走 `SG_IO`；macOS 用只读侦察类型，任何盘操作立即 `Unavailable`。
#[cfg(target_os = "macos")]
pub type PlatformTransport = magi_transport::macos::MacOsDiscovery;

/// 平台命令通道（§4.5）：Linux 走 `SG_IO`；macOS 用只读侦察类型，任何盘操作立即 `Unavailable`。
#[cfg(not(target_os = "macos"))]
pub type PlatformTransport = magi_transport::linux::LinuxSgIo;

/// 一次设备作业的输入（设备标识、身份与传输目标）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceJob {
    /// 设备稳定标识（Linux 上是 `/dev/sgN`，macOS 上是 `VID:PID`）。
    pub device: DeviceId,
    /// 设备身份（§4.1：由 VID/PID 裁决）。
    pub identity: DeviceIdentity,
    /// 传输目标。
    pub target: DeviceTarget,
    /// USB 厂商号。
    pub vid: u16,
    /// USB 产品号。
    pub pid: u16,
    /// Linux 上的设备节点路径。
    pub node: Option<String>,
}

/// 作业取消标志（§6：关闭窗口或点「取消」→ 当前命令返回后停止后续步骤，不做命令级中断）。
#[derive(Debug, Clone, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    /// 未取消。
    pub fn new() -> Self {
        CancelFlag::default()
    }

    /// 置位取消标志（幂等）。
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// 是否已被取消。
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// 取消探针（交给协议层编排的 `cancel` 参数）。
    pub fn probe(&self) -> impl Fn() -> bool + use<> {
        let flag = self.clone();
        move || flag.cancelled()
    }
}

/// 进度上报器：把 §4.7 的 7 个步骤转成 [`AppEvent::Progress`]（§4.13）。
struct EventReporter<'a> {
    emit: &'a dyn Fn(AppEvent),
}

impl ProgressReporter for EventReporter<'_> {
    fn step(&mut self, step: UnlockStep) {
        (self.emit)(AppEvent::Progress { step });
    }
}

/// 扫描本机的 T7 Shield（§4.1/§4.5）：Linux 走 sysfs 扫描；macOS 走只读描述符侦察。
pub fn scan_devices() -> Result<Vec<ScanHit>, AppError> {
    platform_scan()
}

/// 周期重扫间隔（§4.1 REQ-001：设备接入后自动识别）。
///
/// 2 s：远小于人手插拔的最小间隔，插入后最多一个间隔即呈现；扫描在工作线程执行，
/// 主线程只收事件，不占「单帧 ≤ 100 ms」的预算（§6）。常量集中定义于此。
pub const RESCAN_INTERVAL: Duration = Duration::from_secs(2);

/// 一轮设备扫描的结果（取数段输出、呈现段输入；跨线程投递给主线程）。
#[derive(Debug, Default)]
pub struct ScanOutcome {
    /// 命中的设备（已裁剪到同时受理上限，§6）。
    pub hits: Vec<ScanHit>,
    /// 扫描失败（呈现段裁决是否打扰用户：权威轮次呈现，周期轮次只记诊断）。
    pub error: Option<AppError>,
}

/// 取数段：扫描本机设备并裁剪到受理上限（§4.1/§6；诊断环线程安全，工作线程可记录）。
pub fn fetch_scan() -> ScanOutcome {
    match scan_devices() {
        Ok(hits) => {
            let (hits, limit_key) = controller::clamp_devices(hits);
            if let Some(key) = limit_key {
                // §6：同时受理设备上限 8 个，超出部分不呈现并给出提示。
                crate::diagnostics::ring().record(crate::diagnostics::Level::Warn, key);
            }
            ScanOutcome { hits, error: None }
        }
        Err(error) => ScanOutcome {
            hits: Vec::new(),
            error: Some(error),
        },
    }
}

/// 启动周期设备监控线程（§4.1 REQ-001 热插拔感知）：每 [`RESCAN_INTERVAL`] 取数一轮，
/// 把结果经 `sender` 投回主线程呈现；接收端关闭（主循环退出）后线程自行结束。
///
/// 线程创建失败返回 `AppError::Transport(Platform)`（§5，与 [`run_device_job`] 同口径），
/// 不启动监控。
pub fn spawn_scan_watch(sender: async_channel::Sender<ScanOutcome>) -> Result<(), AppError> {
    let spawned = thread::Builder::new()
        .name(String::from("t7-device-watch"))
        .spawn(move || loop {
            thread::sleep(RESCAN_INTERVAL);
            // 接收端已随主循环退出 → 结束监控线程（句柄即弃，线程自行收尾）。
            if sender.send_blocking(fetch_scan()).is_err() {
                break;
            }
        });
    spawned.map(drop).map_err(|err| {
        AppError::Transport(TransportError::Platform {
            code: err.raw_os_error().map(i64::from).unwrap_or(-1),
        })
    })
}

/// macOS：只读描述符侦察（不打开设备、不 claim、不发 CDB）。
#[cfg(target_os = "macos")]
fn platform_scan() -> Result<Vec<ScanHit>, AppError> {
    let mut hits = Vec::new();
    let mut first_error = None;
    let mut succeeded = 0usize;
    // 锁定态与解锁态是两个不同的 PID，分别侦察（§4.1）：某一 PID 下没有设备不是错误，
    // 因此逐个 PID 容忍失败；只有两个 PID 都失败时才把首个错误上报。
    for pid in [magi_protocol::PID_LOCKED, magi_protocol::PID_UNLOCKED] {
        match magi_transport::macos::MacOsDiscovery::enumerate(VENDOR_ID, pid) {
            Ok(summaries) => {
                succeeded += 1;
                for summary in summaries {
                    if let Some(hit) = hit_from_usb(summary.vid, summary.pid, None, Some(&summary))
                    {
                        hits.push(hit);
                    }
                }
            }
            Err(err) => {
                first_error.get_or_insert(err);
            }
        }
    }
    match first_error {
        Some(err) if succeeded == 0 => Err(err.into()),
        _ => Ok(hits),
    }
}

/// Linux：sysfs 设备扫描（只按厂商过滤，PID 判态交给 `identify_device`）。
#[cfg(not(target_os = "macos"))]
fn platform_scan() -> Result<Vec<ScanHit>, AppError> {
    let found = magi_transport::linux::scan::scan_devices(VENDOR_ID)?;
    Ok(found
        .into_iter()
        .filter_map(|device| hit_from_usb(device.vid, device.pid, Some(device.node), None))
        .collect())
}

/// 一次扫描命中（设备卡片与作业的输入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanHit {
    /// 作业输入。
    pub job: DeviceJob,
    /// 只读描述符侦察摘要（macOS 有内容，Linux 为 `None`）。
    pub descriptor: Option<String>,
}

/// 由 USB 标识构造扫描命中：非目标 PID 返回 `None`（§4.1：不识别、不发任何命令）。
pub fn hit_from_usb(
    vid: u16,
    pid: u16,
    node: Option<String>,
    descriptor: Option<&UsbDescriptorSummary>,
) -> Option<ScanHit> {
    let identity = DeviceIdentity::from_ids(vid, pid);
    if identity == DeviceIdentity::Unrecognized {
        return None;
    }
    let device = match &node {
        Some(node) => DeviceId::new(node.clone()),
        None => DeviceId::new(format!("{vid:04x}:{pid:04x}")),
    };
    let target = match &node {
        Some(node) => DeviceTarget::LinuxSg(node.clone()),
        None => DeviceTarget::MacOsUsb { vid, pid },
    };
    Some(ScanHit {
        job: DeviceJob {
            device,
            identity,
            target,
            vid,
            pid,
            node,
        },
        descriptor: descriptor.map(describe_descriptor),
    })
}

/// 描述符摘要（§4.5：备用设置 proto `0x50`（BOT）/`0x62`（UAS）与端点；文案走 i18n 键）。
pub fn describe_descriptor(summary: &UsbDescriptorSummary) -> String {
    let mut parts = Vec::new();
    for setting in &summary.alternate_settings {
        let kind = match setting.protocol {
            0x50 => t!("device.descriptor_bot").to_string(),
            0x62 => t!("device.descriptor_uas").to_string(),
            other => t!("device.descriptor_other", proto = format!("0x{other:02x}")).to_string(),
        };
        let endpoints: Vec<String> = setting
            .endpoints
            .iter()
            .map(|endpoint| format!("{:#04x}", endpoint.address))
            .collect();
        parts.push(format!(
            "{kind} #{} alt {} {}=[{}]",
            setting.interface_number,
            setting.alternate_setting,
            t!("device.descriptor_endpoints"),
            endpoints.join(",")
        ));
    }
    parts.join("; ")
}

/// 解锁作业（§4.7/§4.8）：Discovery → StartSession → 事务 → EndSession → 重枚举观察。
///
/// 取消（§6）：`cancel` 置位时在当前命令返回后停止后续步骤，并尽力 EndSession；此时返回
/// `Ok(None)`，由调用方呈现「已由用户取消」，错误分类不变。协议层零自动重放（D11）。
pub fn unlock_device(
    job: &DeviceJob,
    password: Password,
    cancel: &CancelFlag,
    emit: &dyn Fn(AppEvent),
) -> Result<Option<UnlockEvidence>, AppError> {
    let mut password = password;
    let (transport, discovery) =
        controller::open_and_discover::<DiagnosticsTransport>(job.identity, &job.target)?;
    let flags_before = discovery.locking.raw;

    let probe = cancel.probe();
    let mut reporter = EventReporter { emit };
    let mut session = run_unlock(
        &transport,
        discovery.base_comid,
        &mut password,
        &mut reporter,
        &probe,
    )
    .map_err(AppError::from)?;

    if cancel.cancelled() {
        // §6：尽力关闭会话；清理结果只记录，不改变既有分类，也不阻塞退出。
        if let Err(err) = session.abort(transport.inner()) {
            crate::diagnostics::ring().record(
                crate::diagnostics::Level::Warn,
                crate::presentation::presentation_code(&err.into()),
            );
        }
        crate::diagnostics::ring().record(crate::diagnostics::Level::Info, "user-cancelled");
        return Ok(None);
    }

    // §4.8：解锁序列收尾后观察 30 s（500 ms 间隔、≤60 次）；不发送任何重枚举触发命令。
    let vid = job.vid;
    let (_, evidence) = crate::observation::observe_and_evaluate(
        || magi_transport::linux::scan::observe_reenumeration(vid),
        flags_before,
        || read_locking_flags(job),
    );
    Ok(evidence)
}

/// 口令校验作业（§4.9）：Discovery → StartSession → EndSession；不进入事务、不改动盘上状态。
///
/// 口令被拒返回 [`AppError::PasswordRejected`]（§5：仅此分类允许用户重试）。
pub fn validate_password(
    job: &DeviceJob,
    password: Password,
    emit: &dyn Fn(AppEvent),
) -> Result<Option<UnlockEvidence>, AppError> {
    let mut password = password;
    let _ = emit;
    let (transport, discovery) =
        controller::open_and_discover::<DiagnosticsTransport>(job.identity, &job.target)?;
    let outcome = run_validate_password(&transport, discovery.base_comid, &mut password)
        .map_err(AppError::from)?;
    if !outcome.accepted {
        return Err(AppError::PasswordRejected);
    }
    Ok(None)
}

/// §4.8 判据②：重读一次 Discovery 的 Locking flags。
///
/// 重枚举后原设备节点可能已更换，因此按 VID 重新扫描取当前节点；尽力而为——任何一步失败都
/// 返回 `None`（判据②不成立），不影响判据①与③，也不重试、不轮询。macOS 上无 sysfs，
/// 该路径自然返回 `None`（macOS 分支本就不进入观察窗口）。
fn read_locking_flags(job: &DeviceJob) -> Option<u8> {
    let candidates = magi_transport::linux::scan::scan_devices(job.vid).ok()?;
    let node = candidates
        .into_iter()
        .find(|device| device.pid == magi_protocol::PID_UNLOCKED)
        .map(|device| device.node)?;
    let target = DeviceTarget::LinuxSg(node);
    let transport = PlatformTransport::open(&target).ok()?;
    magi_protocol::discover(&transport)
        .ok()
        .map(|discovery| discovery.locking.raw)
}

/// 记录用传输：诊断缓冲只收结构字段（CDB、方向、响应长度判据）。
type DiagnosticsTransport = crate::diagnostics::RecordingTransport<PlatformTransport>;

/// 设备身份的当前状态键（设备卡片用）。
pub fn identity_for_state(state: Option<DeviceState>) -> DeviceIdentity {
    match state {
        Some(DeviceState::Locked) => DeviceIdentity::Locked,
        Some(DeviceState::Unlocked) => DeviceIdentity::Unlocked,
        Some(DeviceState::ReEnumerating) => DeviceIdentity::ReEnumerating,
        None => DeviceIdentity::Unrecognized,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::DeviceIdentity;
    use crate::test_support::{response_frame, FakeTransport, START_SESSION_BODY_ACCEPTED};
    use magi_protocol::{UnlockStep, PID_LOCKED, PID_UNLOCKED};

    /// 扫描命中映射（§4.1）：两个目标 PID 各归其态，其它 PID 不入列。
    #[test]
    fn test_scan_hit_maps_identity() {
        let locked = hit_from_usb(VENDOR_ID, PID_LOCKED, Some("/dev/sg1".to_string()), None)
            .expect("锁定态必须入列");
        assert_eq!(locked.job.identity, DeviceIdentity::Locked);
        assert_eq!(locked.job.device, DeviceId::new("/dev/sg1"));
        assert_eq!(
            locked.job.target,
            DeviceTarget::LinuxSg("/dev/sg1".to_string())
        );

        let unlocked = hit_from_usb(VENDOR_ID, PID_UNLOCKED, Some("/dev/sg2".to_string()), None)
            .expect("解锁态必须入列");
        assert_eq!(unlocked.job.identity, DeviceIdentity::Unlocked);
        assert_eq!(unlocked.job.device, DeviceId::new("/dev/sg2"));

        assert!(hit_from_usb(VENDOR_ID, 0x61ff, Some("/dev/sg3".to_string()), None).is_none());
        assert!(hit_from_usb(0x1234, PID_LOCKED, None, None).is_none());
    }

    /// macOS 形态的命中：无设备节点 → 目标为 `MacOsUsb`，设备标识为 `VID:PID`。
    #[test]
    fn test_scan_hit_without_device_node_uses_vid_pid() {
        let hit = hit_from_usb(VENDOR_ID, PID_LOCKED, None, None).expect("必须入列");
        assert_eq!(hit.job.device, DeviceId::new("04e8:61fc"));
        assert_eq!(
            hit.job.target,
            DeviceTarget::MacOsUsb {
                vid: VENDOR_ID,
                pid: PID_LOCKED
            }
        );
        assert_eq!(hit.job.node, None);
    }

    /// macOS 盘操作：第一条命令即 `Unavailable`，不产生任何进度、不重试（§4.5/AC-004）。
    #[cfg(target_os = "macos")]
    #[test]
    fn test_macos_disk_operation_returns_unavailable() {
        let job = hit_from_usb(VENDOR_ID, PID_LOCKED, None, None)
            .expect("锁定态必须入列")
            .job;
        let password = crate::controller::validate_password_input("secret").expect("非空口令");
        let cancel = CancelFlag::new();
        let events = RefCell::new(Vec::new());
        let emit = |event: AppEvent| events.borrow_mut().push(event);

        let error =
            unlock_device(&job, password, &cancel, &emit).expect_err("macOS 上盘操作必须立即失败");
        assert_eq!(error, AppError::Transport(TransportError::Unavailable));
        assert_eq!(
            crate::presentation::presentation_code(&error),
            "TransportUnavailable"
        );
        assert!(
            events.borrow().is_empty(),
            "macOS 上不得产生进度事件（未下发任何命令）"
        );
    }

    /// 锚点（W13）：取消在中途置位后不再下发后续命令，并尽力 EndSession，结果标记为用户取消。
    #[test]
    fn test_cancel_records_user_cancel() {
        let comid: u16 = 0x1004; // 测试局部变量（D03）
        let cancel = CancelFlag::new();
        let cancel_for_job = cancel.clone();
        // 取消失败路径：作业在 StartSession 之后置位取消，协议层随即停止后续步骤。
        let inner = FakeTransport::with_responses(vec![
            response_frame(comid, &START_SESSION_BODY_ACCEPTED),
            // 尽力 EndSession 的应答（`FA` 单字节）。
            response_frame(comid, &[0xfa]),
        ]);
        let ring = crate::diagnostics::ring();
        let transport = crate::diagnostics::RecordingTransport::new(inner, Arc::clone(&ring));
        let mut password = crate::controller::validate_password_input("secret").expect("非空口令");
        let mut reporter = EventReporter { emit: &|_event| {} };
        // 先置位取消：StartSession 之前的最后一次 cancel() 检查会立即返回，只留下已建立的会话。
        cancel_for_job.cancel();
        let probe = cancel_for_job.probe();
        let mut session = run_unlock(&transport, comid, &mut password, &mut reporter, &probe)
            .expect("取消不是错误");

        // 取消路径：不进入事务，只尽力 EndSession（FA 是最后一条 OUT）。
        assert_eq!(
            transport.inner().outbound_count(),
            1,
            "取消后不得下发事务命令"
        );
        session
            .abort(transport.inner())
            .expect("尽力 EndSession 必须成功");
        assert_eq!(transport.inner().outbound_count(), 2);
        assert!(cancel.cancelled());
    }

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
