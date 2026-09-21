//! §4.13 线程模型：工作线程 + channel + `AppEvent` 投递。
//!
//! - 协议 I/O 一律在工作线程执行，UI 主线程只做事件消费（§6：主线程单帧阻塞 ≤ 100 ms）；
//! - 同设备同时最多 1 个在飞作业（复用 [`JobRegistry`]；命令队列上限 1，
//!   溢出即拒绝，不排队）；
//! - 事件经 `async_channel` 交还调用窗口：[`spawn_device_job`] 返回事件通道，由调用
//!   窗口在自己的 `glib::MainContext` 消费循环里直连消费（窗口自持，无全局事件汇）。

use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use async_channel::Receiver;
use magi_protocol::{
    run_unlock, run_validate_password, DeviceState, Discovery, Password, ProgressReporter,
    RunError, UnlockEvidence, UnlockStep, VENDOR_ID,
};
use magi_transport::transport::{DeviceTarget, Transport, TransportError};
use magi_transport::usb_descriptor::UsbDescriptorSummary;
use rust_i18n::t;

use crate::device::rescan::{clamp_devices, DeviceIdentity};
use crate::presentation::{AppError, AppEvent};

/// 设备稳定标识（Linux 上是 `/dev/sgN`）。
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

impl From<OperationStartError> for AppError {
    fn from(err: OperationStartError) -> Self {
        match err {
            OperationStartError::App(err) => err,
            // 界面只对已识别设备入队（`jobs::hit_from_usb` 过滤非目标 PID），此处是兜底：
            // 「未识别」在协议层的等价事实是该设备不提供 Opal SSC 通道（§5）。
            OperationStartError::DeviceNotRecognized => {
                AppError::Protocol(magi_protocol::ProtocolError::NoOpalSscDescriptor)
            }
        }
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
    let discovery = magi_protocol::discover(&transport)?;
    Ok((transport, discovery))
}


/// 全局单飞注册表：每设备 1 个工作线程（§6）。
static REGISTRY: LazyLock<JobRegistry> = LazyLock::new(JobRegistry::new);
/// 是否有任一设备作业在飞（§4.13：周期重扫在飞轮次只记在位缓存，不改呈现）。
pub fn any_job_in_flight() -> bool {
    REGISTRY.in_flight() > 0
}

/// 在工作线程执行 `job`：`job` 用给定的发射器上报 [`AppEvent`]，返回值为收尾证据。
///
/// 返回事件通道与工作线程句柄；调用方负责消费通道（[`spawn_device_job`] 把通道交还调用窗口）。
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

/// §4.13：在工作线程执行 `job`，返回事件通道供调用方在主线程消费。
///
/// 同设备最多一个在飞任务；已有在飞任务时返回 `AppError::Busy`。
pub fn spawn_device_job<F>(dev: DeviceId, job: F) -> Result<Receiver<AppEvent>, AppError>
where
    F: FnOnce(&dyn Fn(AppEvent)) -> Result<Option<UnlockEvidence>, AppError> + Send + 'static,
{
    Ok(run_device_job(dev, job)?.0)
}

/// 平台命令通道（D27：仅 Linux）：`SG_IO`；非 Linux 平台上 `open` 返回 `Unavailable`。
pub type PlatformTransport = magi_transport::linux::LinuxSgIo;

/// 一次设备作业的输入（设备标识、身份与传输目标）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceJob {
    /// 设备稳定标识（Linux 上是 `/dev/sgN`）。
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

/// 扫描本机的 T7 Shield（§4.1）：走 sysfs 扫描。
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
            let (hits, limit_key) = clamp_devices(hits);
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

/// 本机设备扫描：sysfs 设备扫描（只按厂商过滤，PID 判态交给 `identify_device`）。
fn platform_scan() -> Result<Vec<ScanHit>, AppError> {
    let found = magi_transport::linux::scan::scan_devices(VENDOR_ID)?;
    Ok(found
        .into_iter()
        .filter_map(|device| hit_from_usb(device.vid, device.pid, device.node, None))
        .collect())
}

/// 一次扫描命中（设备卡片与作业的输入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanHit {
    /// 作业输入。
    pub job: DeviceJob,
    /// 描述符摘要（调用方注入时呈现；当前 Linux 扫描路径为 `None`）。
    pub descriptor: Option<String>,
}

/// 由 USB 标识与设备节点构造扫描命中：非目标 PID 返回 `None`（§4.1：不识别、不发任何命令）。
pub fn hit_from_usb(
    vid: u16,
    pid: u16,
    node: String,
    descriptor: Option<&UsbDescriptorSummary>,
) -> Option<ScanHit> {
    let identity = DeviceIdentity::from_ids(vid, pid);
    if identity == DeviceIdentity::Unrecognized {
        return None;
    }
    Some(ScanHit {
        job: DeviceJob {
            device: DeviceId::new(node.clone()),
            identity,
            target: DeviceTarget::LinuxSg(node.clone()),
            vid,
            pid,
            node: Some(node),
        },
        descriptor: descriptor.map(describe_descriptor),
    })
}

/// 描述符摘要（备用设置 proto `0x50`（BOT）/`0x62`（UAS）与端点；文案走 i18n 键）。
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
        open_and_discover::<DiagnosticsTransport>(job.identity, &job.target)?;
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
    let (_, evidence) = crate::device::observation::observe_and_evaluate(
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
        open_and_discover::<DiagnosticsTransport>(job.identity, &job.target)?;
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
/// 返回 `None`（判据②不成立），不影响判据①与③，也不重试、不轮询。
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
    use crate::device::gate::allowed_actions;
    use crate::device::rescan::DeviceIdentity;
    use crate::test_support::{response_frame, FakeTransport, START_SESSION_BODY_ACCEPTED};
    use magi_protocol::{identify_device, UnlockStep, PID_LOCKED, PID_UNLOCKED};

    /// 扫描命中映射（§4.1）：两个目标 PID 各归其态，其它 PID 不入列。
    #[test]
    fn test_scan_hit_maps_identity() {
        let locked = hit_from_usb(VENDOR_ID, PID_LOCKED, "/dev/sg1".to_string(), None)
            .expect("锁定态必须入列");
        assert_eq!(locked.job.identity, DeviceIdentity::Locked);
        assert_eq!(locked.job.device, DeviceId::new("/dev/sg1"));
        assert_eq!(
            locked.job.target,
            DeviceTarget::LinuxSg("/dev/sg1".to_string())
        );

        let unlocked = hit_from_usb(VENDOR_ID, PID_UNLOCKED, "/dev/sg2".to_string(), None)
            .expect("解锁态必须入列");
        assert_eq!(unlocked.job.identity, DeviceIdentity::Unlocked);
        assert_eq!(unlocked.job.device, DeviceId::new("/dev/sg2"));

        assert!(hit_from_usb(VENDOR_ID, 0x61ff, "/dev/sg3".to_string(), None).is_none());
        assert!(hit_from_usb(0x1234, PID_LOCKED, "/dev/sg4".to_string(), None).is_none());
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
        let mut password = crate::device::gate::validate_password_input("secret").expect("非空口令");
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
            _cdb: &magi_transport::transport::ScsiCdb,
            _dir: magi_transport::transport::Direction,
            _data: &mut [u8],
            _timeout: Duration,
        ) -> Result<usize, TransportError> {
            Err(TransportError::Unavailable)
        }
    }

}
