//! §3.3 会话状态机与命令编排：discovery → StartSession → StartTransaction + 4×`Set`
//! + EndTransaction → EndSession，以及中止路径（尽力 EndSession）与 REQ-011 占位错误。
//!
//! 编排规则（§4.3/§4.7/§5）：
//! - 每条 TCG 命令 = 一次 OUT（`cdb_security_out`）+ 一次 IN（`cdb_security_in`，固定
//!   2048 B 分配）；discovery 只发 IN（SP specific 固定 `0x0001`，分配 4096 B）。
//! - `FB`/`FC`/`FA` 的空应答不作为失败判据（设备以裸包确认），`Set` 类的空应答判致命。
//! - 协议层零自动重放：任何失败都不重发命令；取消只停止后续步骤（§6），已建立会话由
//!   调用方经 [`UnlockSession::abort`] 尽力关闭。
//! - 不发送任何重枚举触发命令（§4.8/AC-007；客户端不得下发该命令）。

use std::fmt;

use t7_transport::cdb::{
    cdb_security_in, cdb_security_out, CMD_TIMEOUT, DISCOVERY_ALLOC_LEN, TCG_ALLOC_LEN,
};
use t7_transport::transport::{Direction, Transport, TransportError};
use zeroize::Zeroizing;

use crate::discovery::{self, DeviceState, Discovery};
use crate::error::{CommandStep, ProtocolError, UnlockStep};
use crate::frame::{
    declared_data_len, parse_response, StatusListForm, TcgResponse, END_SESSION_RESPONSE_LEN,
    END_TRANSACTION_RESPONSE_LEN, PAYLOAD_HEADER, SET_RESPONSE_LEN, START_SESSION_RESPONSE_LEN,
    START_TRANSACTION_RESPONSE_LEN,
};
use crate::password::Password;
use crate::session::{self, SessionIds, ValidateOutcome};
use crate::transaction::{self, UnlockSetOp};

/// §4.3：唯一允许的 SECURITY PROTOCOL 字节（A 路 TCG）。本层不提供传其它协议字节的入口。
pub const SECURITY_PROTOCOL_TCG: u8 = 0x01;

/// §3.2 客户端会话态：一次操作从 `Idle` 走到 `Closed` 或 `Failed`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// 空闲：未做 discovery。
    Idle,
    /// discovery 成功，ComID 与 Locking flags 就绪。
    Discovered,
    /// StartSession 成功，会话号已就绪。
    SessionOpen,
    /// StartTransaction 已发送。
    InTransaction,
    /// EndSession 成功。
    Closed,
    /// 任一环节返回不可恢复错误。
    Failed,
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            SessionState::Idle => "Idle",
            SessionState::Discovered => "Discovered",
            SessionState::SessionOpen => "SessionOpen",
            SessionState::InTransaction => "InTransaction",
            SessionState::Closed => "Closed",
            SessionState::Failed => "Failed",
        };
        f.write_str(name)
    }
}

/// §3.2 一次操作的上下文；`base_comid` 只能来自 Discovery 解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationContext {
    pub device_state: DeviceState,
    pub session_state: SessionState,
    /// `None` 时禁止发送任何需要 ComID 的命令（§3.2 零值规则）。
    pub base_comid: Option<u16>,
    /// 报文头 `+0x14`：设备会话号（StartSession 应答 token[5] 的 4 字节映像）。
    pub tsn: [u8; 4],
    /// 报文头 `+0x18`：主机会话号（StartSession 应答 token[4] 的 4 字节映像）。
    pub hsn: [u8; 4],
}

/// 运行错误：与 §4.13 的 `AppError::Transport` / `AppError::Protocol` 一一对应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    Transport(TransportError),
    Protocol(ProtocolError),
}

impl From<TransportError> for RunError {
    fn from(err: TransportError) -> Self {
        RunError::Transport(err)
    }
}

impl From<ProtocolError> for RunError {
    fn from(err: ProtocolError) -> Self {
        RunError::Protocol(err)
    }
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunError::Transport(err) => write!(f, "传输错误：{err:?}"),
            RunError::Protocol(err) => write!(f, "协议错误：{err}"),
        }
    }
}

/// 进度上报（§4.13）：只覆盖 §4.7 的 7 条命令。
pub trait ProgressReporter {
    fn step(&mut self, step: UnlockStep);
}

/// 一次已建立会话的操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlockSession {
    ids: SessionIds,
    form: StatusListForm,
    context: OperationContext,
}

impl UnlockSession {
    /// 新建一次解锁/校验操作的上下文（`comid == 0` 视为未取得 ComID，§3.2 零值规则）。
    ///
    /// 设备态记录为 [`DeviceState::Locked`]：解锁与口令校验只在锁定态发起（§3.3）。
    fn new(comid: u16) -> Result<Self, RunError> {
        if comid == 0 {
            return Err(RunError::Protocol(ProtocolError::NoOpalSscDescriptor));
        }
        Ok(UnlockSession {
            ids: SessionIds {
                tsn: [0, 0, 0, 0],
                hsn: [0, 0, 0, 0],
            },
            form: StatusListForm::Single,
            context: OperationContext {
                device_state: DeviceState::Locked,
                session_state: SessionState::Discovered,
                base_comid: Some(comid),
                tsn: [0, 0, 0, 0],
                hsn: [0, 0, 0, 0],
            },
        })
    }

    /// 会话号（§4.6；StartSession 成功后才有非零值）。
    pub fn ids(&self) -> SessionIds {
        self.ids
    }

    /// 本次操作的上下文（§3.2）。
    pub fn context(&self) -> &OperationContext {
        &self.context
    }

    /// 客户端会话态（§3.3）。
    pub fn state(&self) -> SessionState {
        self.context.session_state
    }

    /// 尽力关闭会话（§3.3 中止路径）：仅在会话仍打开时发送 EndSession，成功 → `Closed`、
    /// 失败 → `Failed`；已关闭或已失败时不重发任何命令（零自动重放）。
    ///
    /// 返回的清理错误只由调用方记录，不覆盖本次操作已有的错误分类。
    pub fn abort<T: Transport>(&mut self, t: &T) -> Result<(), RunError> {
        if !matches!(
            self.context.session_state,
            SessionState::SessionOpen | SessionState::InTransaction
        ) {
            return Ok(());
        }
        let Ok(comid) = self.comid() else {
            self.context.session_state = SessionState::Failed;
            return Ok(());
        };
        let payload = transaction::end_session_payload(comid, &self.ids, self.form);
        match self.send_control(t, payload, END_SESSION_RESPONSE_LEN, UnlockStep::EndSession) {
            Ok(()) => {
                self.context.session_state = SessionState::Closed;
                Ok(())
            }
            Err(err) => {
                self.context.session_state = SessionState::Failed;
                Err(err)
            }
        }
    }

    fn comid(&self) -> Result<u16, RunError> {
        self.context
            .base_comid
            .ok_or(RunError::Protocol(ProtocolError::NoOpalSscDescriptor))
    }

    /// §4.6：构造 StartSession 载荷 → 立即 zeroize 口令缓冲 → OUT/IN → 长度与方法状态字节
    /// 判据（§5）→ 会话号映射（§4.6）。
    fn start_session<T: Transport>(
        &mut self,
        t: &T,
        pwd: &mut Password,
    ) -> Result<ValidateOutcome, RunError> {
        let comid = self.comid()?;
        let mut payload = {
            let req = session::locking_sp_request(comid, pwd.expose());
            Zeroizing::new(session::start_session_payload(&req))
        };
        // §6 安全约束：口令缓冲在报文构造完成后立即清零（报文本身在发送后随 Zeroizing 清零）。
        pwd.zeroize_now();
        let raw = exchange(t, comid, payload.as_mut_slice())?;
        drop(payload);

        let resp = raw.parse(START_SESSION_RESPONSE_LEN, CommandStep::StartSession)?;
        let outcome = session::validate_outcome(&resp)?;
        // §4.7：StartSession 应答的回显决定本次操作的状态列表形态，判定一次、操作内不变。
        self.form = crate::frame::detect_status_list_form(&resp);
        if outcome.accepted {
            let ids = session::session_ids_from_response(&resp)?;
            self.ids = ids;
            self.context.tsn = ids.tsn;
            self.context.hsn = ids.hsn;
        }
        Ok(outcome)
    }

    /// `FB`/`FC`/`FA`：空应答不作为失败判据，其余长度与方法状态字节按 §5 判定。
    fn send_control<T: Transport>(
        &mut self,
        t: &T,
        mut payload: Vec<u8>,
        expected: usize,
        step: UnlockStep,
    ) -> Result<(), RunError> {
        let comid = self.comid()?;
        let raw = exchange(t, comid, payload.as_mut_slice())?;
        let resp = raw.parse(expected, step.into())?;
        if resp.data_len() == 0 {
            return Ok(());
        }
        resp.expect_data_len(expected, step)?;
        self.status_ok(&resp, expected, step)
    }

    /// `Set`：空应答致命（`EmptyResponse`），长度期望 8（§4.7）。
    fn send_set<T: Transport>(
        &mut self,
        t: &T,
        mut payload: Vec<u8>,
        step: UnlockStep,
    ) -> Result<(), RunError> {
        let comid = self.comid()?;
        let raw = exchange(t, comid, payload.as_mut_slice())?;
        let resp = raw.parse(SET_RESPONSE_LEN, step.into())?;
        resp.expect_set_response(step)?;
        self.status_ok(&resp, SET_RESPONSE_LEN, step)
    }

    fn status_ok(
        &self,
        resp: &TcgResponse<'_>,
        expected: usize,
        step: UnlockStep,
    ) -> Result<(), RunError> {
        let status_byte = resp.require_status_byte(expected, step)?;
        if status_byte == 0 {
            Ok(())
        } else {
            // §5：状态列表内方法状态字节非 0 即被拒（口令错误实测为 1）。
            Err(RunError::Protocol(ProtocolError::SessionRejected {
                status_byte,
            }))
        }
    }
}

/// Level-0 Discovery（§4.2）：唯一不携带 ComID 的命令。
pub fn discover<T: Transport>(t: &T) -> Result<Discovery, RunError> {
    let cdb = discovery::discovery_cdb();
    let mut buf = [0u8; DISCOVERY_ALLOC_LEN as usize];
    let got = t
        .execute(&cdb, Direction::In, &mut buf, CMD_TIMEOUT)
        .map_err(classify_transport_error)?;
    let got = got.min(buf.len());
    discovery::parse_level0(&buf[..got]).map_err(RunError::Protocol)
}

/// §4.7 解锁全流程：StartSession → StartTransaction + 4×`Set` + EndTransaction → EndSession。
///
/// `cancel()` 为真时，在**当前命令返回后**停止后续步骤（§6 取消语义）并返回仍打开的会话，
/// 由调用方经 [`UnlockSession::abort`] 尽力 EndSession；失败路径由本函数尽力关闭会话。
/// 协议层不自动重放任何命令（D11）。
pub fn run_unlock<T: Transport>(
    t: &T,
    comid: u16,
    pwd: &mut Password,
    reporter: &mut dyn ProgressReporter,
    cancel: &dyn Fn() -> bool,
) -> Result<UnlockSession, RunError> {
    let mut session = UnlockSession::new(comid)?;
    match unlock_sequence(t, &mut session, pwd, reporter, cancel) {
        Ok(()) => Ok(session),
        Err(err) => {
            // §5/§3.3：判致命即终止序列并尽力关闭会话；清理失败只记录，不改变错误分类。
            let _ = session.abort(t);
            Err(err)
        }
    }
}

/// §4.9 口令校验：复用与解锁相同的 StartSession，区别只在不发 StartTransaction，
/// 收尾只发 EndSession。
///
/// 返回 `ValidateOutcome`（`accepted = false` 表示口令被拒，不改变盘上状态）；
/// 只有传输层/协议层事实错误才返回 `Err`。
pub fn run_validate_password<T: Transport>(
    t: &T,
    comid: u16,
    pwd: &mut Password,
) -> Result<ValidateOutcome, RunError> {
    let mut session = UnlockSession::new(comid)?;
    let outcome = session.start_session(t, pwd)?;
    if outcome.accepted {
        session.context.session_state = SessionState::SessionOpen;
        session.abort(t)?;
    }
    // §3.3：被拒时（方法状态字节非 0）会话未曾建立，没有收尾动作，也不重发任何命令。
    Ok(outcome)
}

fn unlock_sequence<T: Transport>(
    t: &T,
    session: &mut UnlockSession,
    pwd: &mut Password,
    reporter: &mut dyn ProgressReporter,
    cancel: &dyn Fn() -> bool,
) -> Result<(), RunError> {
    let comid = session.comid()?;
    let outcome = session.start_session(t, pwd)?;
    if !outcome.accepted {
        session.context.session_state = SessionState::Failed;
        return Err(RunError::Protocol(ProtocolError::SessionRejected {
            status_byte: outcome.status_byte,
        }));
    }
    session.context.session_state = SessionState::SessionOpen;
    let ids = session.ids;
    // §3.2 零值规则：`tsn`/`hsn` 为零值禁止发送 StartTransaction 及其后的任何命令。
    if ids.tsn == [0, 0, 0, 0] || ids.hsn == [0, 0, 0, 0] {
        session.context.session_state = SessionState::Failed;
        return Err(RunError::Protocol(ProtocolError::SessionIdsMissing));
    }
    let form = session.form;

    if cancel() {
        return Ok(());
    }
    reporter.step(UnlockStep::StartTransaction);
    session.send_control(
        t,
        transaction::start_transaction_payload(comid, &ids, form),
        START_TRANSACTION_RESPONSE_LEN,
        UnlockStep::StartTransaction,
    )?;
    session.context.session_state = SessionState::InTransaction;

    // §4.7 的 4 条 Set（顺序由 `unlock_sets()` 唯一定义），进度步骤取 `UnlockStep::ALL[1..5]`。
    for (index, op) in transaction::unlock_sets().iter().enumerate() {
        let step = UnlockStep::ALL[index + 1];
        if cancel() {
            return Ok(());
        }
        reporter.step(step);
        let payload = match op {
            UnlockSetOp::Cell(cell) => transaction::set_cell_payload(comid, &ids, form, cell),
            UnlockSetOp::Row(row) => transaction::set_row_payload(comid, &ids, form, row),
        };
        session.send_set(t, payload, step)?;
    }

    if cancel() {
        return Ok(());
    }
    reporter.step(UnlockStep::EndTransaction);
    session.send_control(
        t,
        transaction::end_transaction_payload(comid, &ids, form),
        END_TRANSACTION_RESPONSE_LEN,
        UnlockStep::EndTransaction,
    )?;
    session.context.session_state = SessionState::SessionOpen;

    if cancel() {
        return Ok(());
    }
    reporter.step(UnlockStep::EndSession);
    session.send_control(
        t,
        transaction::end_session_payload(comid, &ids, form),
        END_SESSION_RESPONSE_LEN,
        UnlockStep::EndSession,
    )?;
    session.context.session_state = SessionState::Closed;
    Ok(())
}

/// §4.11：口令设置 / 修改 / 删除在字节级证据齐备前不可执行——立即返回错误，
/// 不构造任何令牌流、不下发任何命令。
pub fn set_password(_pwd: &[u8]) -> Result<(), ProtocolError> {
    Err(ProtocolError::PasswordOperationUnspecified)
}

/// §4.11：删除口令同上，不臆造令牌流。
pub fn delete_password(_pwd: &[u8]) -> Result<(), ProtocolError> {
    Err(ProtocolError::PasswordOperationUnspecified)
}

/// sense `03/11/00` 是「通道不存在」的信号（§5）：判定的唯一定义在
/// `t7_transport::sense::SenseData::is_unsupported_security_protocol`，本层只做分类。
///
/// 其余错误原样透传（含 `ShortResponse { got }`，其 `got < 0x38` 由 §4.3 规定。
pub fn classify_transport_error(err: TransportError) -> RunError {
    match err {
        TransportError::ScsiCheckCondition { sense }
            if sense.is_unsupported_security_protocol() =>
        {
            RunError::Protocol(ProtocolError::UnsupportedSecurityProtocol {
                proto: SECURITY_PROTOCOL_TCG,
                sense,
            })
        }
        other => RunError::Transport(other),
    }
}

/// IN 的返回：固定 `TCG_ALLOC_LEN` 缓冲 + 实际字节数（§6：禁止按响应内容动态扩容）。
struct RawResponse {
    buf: [u8; TCG_ALLOC_LEN as usize],
    got: usize,
}

impl RawResponse {
    /// 解析响应头：不足 `0x38` → `TransportError::ShortResponse`（§4.3）；`+0x34` 声明的
    /// `data_len` 越界 → `UnexpectedResponseLength`（§5）。
    fn parse(&self, expected: usize, step: CommandStep) -> Result<TcgResponse<'_>, RunError> {
        let raw = &self.buf[..self.got];
        if raw.len() < PAYLOAD_HEADER {
            return Err(RunError::Transport(TransportError::ShortResponse {
                got: self.got,
            }));
        }
        parse_response(raw).ok_or(RunError::Protocol(
            ProtocolError::UnexpectedResponseLength {
                step,
                expected,
                actual: declared_data_len(raw).unwrap_or(0),
            },
        ))
    }
}

/// 一次 OUT + IN（§4.3）：OUT 载荷即报文，IN 分配长度固定 2048 B。
fn exchange<T: Transport>(t: &T, comid: u16, payload: &mut [u8]) -> Result<RawResponse, RunError> {
    let out_cdb = cdb_security_out(comid, payload.len() as u32);
    t.execute(&out_cdb, Direction::Out, payload, CMD_TIMEOUT)
        .map_err(classify_transport_error)?;

    let in_cdb = cdb_security_in(comid, TCG_ALLOC_LEN);
    let mut buf = [0u8; TCG_ALLOC_LEN as usize];
    let got = t
        .execute(&in_cdb, Direction::In, &mut buf, CMD_TIMEOUT)
        .map_err(classify_transport_error)?;
    Ok(RawResponse {
        buf,
        got: got.min(buf.len()),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::time::Duration;

    use t7_transport::cdb::DISCOVERY_SP_SPECIFIC;
    use t7_transport::sense::SenseData;
    use t7_transport::transport::{DeviceTarget, ScsiCdb};

    use super::*;
    use crate::frame::testkit::{hex, start_session_body, synthetic_response};
    use crate::frame::STATUS_LIST_SINGLE;

    /// 测试用 ComID（D03：实测值只作测试局部变量，不得写进生产路径）。
    const TEST_COMID: u16 = 0x1004;

    /// 记录一次 `execute`（`payload` 为 OUT 的发送数据或 IN 的实际返回数据）。
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Exchange {
        cdb: ScsiCdb,
        dir: Direction,
        data_len: usize,
        payload: Vec<u8>,
    }

    /// 脚本化假传输：OUT 记日志并成功；IN 从脚本取预置应答（可注入传输错误）。
    struct FakeTransport {
        log: RefCell<Vec<Exchange>>,
        script: RefCell<VecDeque<Result<Vec<u8>, TransportError>>>,
        ins: Cell<usize>,
    }

    impl FakeTransport {
        fn new(script: Vec<Result<Vec<u8>, TransportError>>) -> Self {
            FakeTransport {
                log: RefCell::new(Vec::new()),
                script: RefCell::new(script.into()),
                ins: Cell::new(0),
            }
        }

        fn executed_ins(&self) -> usize {
            self.ins.get()
        }

        fn out_payloads(&self) -> Vec<Vec<u8>> {
            self.log
                .borrow()
                .iter()
                .filter(|e| e.dir == Direction::Out)
                .map(|e| e.payload.clone())
                .collect()
        }

        fn ins_len(&self) -> Vec<usize> {
            self.log
                .borrow()
                .iter()
                .filter(|e| e.dir == Direction::In)
                .map(|e| e.data_len)
                .collect()
        }
    }

    impl Transport for FakeTransport {
        fn open(_target: &DeviceTarget) -> Result<Self, TransportError> {
            Ok(FakeTransport::new(Vec::new()))
        }

        fn execute(
            &self,
            cdb: &ScsiCdb,
            dir: Direction,
            data: &mut [u8],
            _timeout: Duration,
        ) -> Result<usize, TransportError> {
            match dir {
                Direction::Out => {
                    self.log.borrow_mut().push(Exchange {
                        cdb: *cdb,
                        dir,
                        data_len: data.len(),
                        payload: data.to_vec(),
                    });
                    Ok(data.len())
                }
                Direction::In => {
                    self.ins.set(self.ins.get() + 1);
                    let scripted = self.script.borrow_mut().pop_front().expect("未预置应答");
                    match scripted {
                        Ok(bytes) => {
                            let n = bytes.len().min(data.len());
                            data[..n].copy_from_slice(&bytes[..n]);
                            self.log.borrow_mut().push(Exchange {
                                cdb: *cdb,
                                dir,
                                data_len: data.len(),
                                payload: bytes,
                            });
                            Ok(n)
                        }
                        Err(err) => {
                            self.log.borrow_mut().push(Exchange {
                                cdb: *cdb,
                                dir,
                                data_len: data.len(),
                                payload: Vec::new(),
                            });
                            Err(err)
                        }
                    }
                }
            }
        }
    }

    #[derive(Default)]
    struct StepLog {
        steps: RefCell<Vec<UnlockStep>>,
    }

    impl StepLog {
        fn steps(&self) -> Vec<UnlockStep> {
            self.steps.borrow().clone()
        }
    }

    impl ProgressReporter for StepLog {
        fn step(&mut self, step: UnlockStep) {
            self.steps.borrow_mut().push(step);
        }
    }

    fn start_session_response(status: u8) -> Vec<u8> {
        synthetic_response(TEST_COMID, &start_session_body(status))
    }

    fn set_response(status: u8) -> Vec<u8> {
        synthetic_response(TEST_COMID, &hex(&format!("f8 00 00 f0 0{status} 00 00 f1")))
    }

    fn ok_script() -> Vec<Result<Vec<u8>, TransportError>> {
        vec![
            Ok(start_session_response(0)),
            Ok(synthetic_response(TEST_COMID, &hex("fb 00"))),
            Ok(set_response(0)),
            Ok(set_response(0)),
            Ok(set_response(0)),
            Ok(set_response(0)),
            Ok(synthetic_response(TEST_COMID, &hex("fc 00"))),
            Ok(synthetic_response(TEST_COMID, &hex("fa"))),
        ]
    }

    fn tokens_of(pkt: &[u8]) -> &[u8] {
        let n = u32::from_be_bytes([pkt[0x34], pkt[0x35], pkt[0x36], pkt[0x37]]) as usize;
        &pkt[PAYLOAD_HEADER..PAYLOAD_HEADER + n]
    }

    /// 一次成功解锁：7 步进度、8 条命令各一次、会话号进报文头、Set 顺序与对象 UID 正确。
    #[test]
    fn test_unlock_sequence_success() {
        let fake = FakeTransport::new(ok_script());
        let mut reporter = StepLog::default();
        let mut pwd = Password::new(b"0123456789abcdef".to_vec());
        let session = run_unlock(&fake, TEST_COMID, &mut pwd, &mut reporter, &|| false)
            .expect("解锁必须成功");

        assert_eq!(reporter.steps(), UnlockStep::ALL.to_vec());
        assert_eq!(session.state(), SessionState::Closed);
        assert_eq!(session.ids().tsn, [0x00, 0x00, 0x10, 0x1A]);
        assert_eq!(session.ids().hsn, [0x00, 0x00, 0x00, 0x01]);
        assert_eq!(session.context().base_comid, Some(TEST_COMID));
        assert!(pwd.is_empty(), "报文构造完成后口令缓冲必须清零");

        let outs = fake.out_payloads();
        assert_eq!(outs.len(), 8, "StartSession + FB + 4×Set + FC + FA");
        assert_eq!(fake.executed_ins(), 8, "每条命令恰好一次 IN（零自动重放）");
        assert!(
            fake.ins_len().iter().all(|n| *n == 0x800),
            "IN 固定 2048 B 分配"
        );

        // 会话内 7 条命令的报文头 +0x14/+0x18 = TSN/HSN（§4.7）。
        for pkt in &outs[1..] {
            assert_eq!(&pkt[0x14..0x18], &[0x00, 0x00, 0x10, 0x1A]);
            assert_eq!(&pkt[0x18..0x1c], &[0x00, 0x00, 0x00, 0x01]);
            assert!(tokens_of(pkt).ends_with(&STATUS_LIST_SINGLE), "单列表形态");
        }
        // §4.7 的 4 条 Set：92 B 载荷、目标对象依次为 MBRControl / LRG / LRG / DataStore。
        let set_uids = [
            crate::uid::MBRCONTROL,
            crate::uid::LOCKINGRANGE_GLOBAL,
            crate::uid::LOCKINGRANGE_GLOBAL,
            crate::uid::DATASTORE,
        ];
        for (index, uid) in set_uids.iter().enumerate() {
            let pkt = &outs[2 + index];
            assert_eq!(pkt.len(), 92);
            assert_eq!(tokens_of(pkt)[1..10], crate::atom::uid_atom(uid)[..]);
        }
        assert_eq!(tokens_of(&outs[1])[..2], [0xFB, 0x00]);
        assert_eq!(tokens_of(&outs[6])[..2], [0xFC, 0x00]);
        assert_eq!(tokens_of(&outs[7])[0], 0xFA);
        assert_eq!(tokens_of(&outs[0])[0], 0xF8, "StartSession 用 CALL 起始");
    }

    /// spec §10 锚点：中止路径尽力 EndSession、失败只记录（不覆盖已有结果、零重放）。
    #[test]
    fn test_session_closed_on_abort() {
        // 第一部分：取消发生在 StartSession 之后 → 停止后续步骤，由调用方尽力 EndSession。
        let fake = FakeTransport::new(vec![
            Ok(start_session_response(0)),
            Ok(synthetic_response(TEST_COMID, &hex("fa"))),
        ]);
        let mut reporter = StepLog::default();
        let mut pwd = Password::new(b"hunter2".to_vec());
        let cancel = || fake.executed_ins() >= 1;
        let mut session =
            run_unlock(&fake, TEST_COMID, &mut pwd, &mut reporter, &cancel).expect("取消不是错误");
        assert_eq!(session.state(), SessionState::SessionOpen);
        assert!(reporter.steps().is_empty(), "取消后不得上报未执行的步骤");
        assert_eq!(fake.out_payloads().len(), 1, "只发出 StartSession");

        let earlier_ids = session.ids();
        session.abort(&fake).expect("EndSession 必须成功");
        assert_eq!(session.state(), SessionState::Closed);
        let outs = fake.out_payloads();
        assert_eq!(outs.len(), 2);
        assert_eq!(
            tokens_of(&outs[1])[0],
            0xFA,
            "最后一条 OUT 必须是 EndSession"
        );
        assert!(tokens_of(&outs[1]).ends_with(&STATUS_LIST_SINGLE));
        assert_eq!(session.ids(), earlier_ids, "中止不得改写已得到的会话号");
        session.abort(&fake).expect("已关闭的会话不再发送命令");
        assert_eq!(
            fake.out_payloads().len(),
            2,
            "零自动重放：不重发 EndSession"
        );

        // 第二部分：EndSession 返回 CHECK CONDITION → abort 返回错误，命令仍各一次。
        let sense = SenseData {
            response_code: 0x70,
            sense_key: 0x05,
            asc: 0x20,
            ascq: 0x00,
        };
        let failing = FakeTransport::new(vec![
            Ok(start_session_response(0)),
            Err(TransportError::ScsiCheckCondition { sense }),
        ]);
        let cancel = || failing.executed_ins() >= 1;
        let mut failed_pwd = Password::new(b"hunter2".to_vec());
        let mut failed_session = run_unlock(
            &failing,
            TEST_COMID,
            &mut failed_pwd,
            &mut StepLog::default(),
            &cancel,
        )
        .expect("取消不是错误");
        assert_eq!(failed_session.state(), SessionState::SessionOpen);
        assert_eq!(
            failed_session.abort(&failing),
            Err(RunError::Transport(TransportError::ScsiCheckCondition {
                sense
            })),
            "中止失败原样返回，不改变已有结果的分类"
        );
        assert_eq!(failed_session.state(), SessionState::Failed);
        assert_eq!(failed_session.ids(), earlier_ids, "失败的中止不改写会话号");
        assert_eq!(
            failing.out_payloads().len(),
            2,
            "StartSession + 一次 EndSession，不重发"
        );
        assert_eq!(failed_session.abort(&failing), Ok(()));
        assert_eq!(failing.out_payloads().len(), 2, "失败后不再重发 EndSession");
    }

    /// spec §10 锚点：sense `03/11/00` → `UnsupportedSecurityProtocol`；CDB 只允许协议字节 `0x01`。
    #[test]
    fn test_unsupported_security_protocol() {
        let sense = SenseData {
            response_code: 0x70,
            sense_key: 0x03,
            asc: 0x11,
            ascq: 0x00,
        };
        assert_eq!(
            classify_transport_error(TransportError::ScsiCheckCondition { sense }),
            RunError::Protocol(ProtocolError::UnsupportedSecurityProtocol {
                proto: SECURITY_PROTOCOL_TCG,
                sense,
            })
        );
        // 其它 sense 原样透传，不被误判为通道不存在。
        let other = SenseData {
            response_code: 0x70,
            sense_key: 0x05,
            asc: 0x20,
            ascq: 0x00,
        };
        assert_eq!(
            classify_transport_error(TransportError::ScsiCheckCondition { sense: other }),
            RunError::Transport(TransportError::ScsiCheckCondition { sense: other })
        );
        assert_eq!(
            classify_transport_error(TransportError::Unavailable),
            RunError::Transport(TransportError::Unavailable)
        );

        // §4.3 只允许两类 CDB，且协议字节固定 0x01（构造入口不接受协议字节参数）。
        assert_eq!(SECURITY_PROTOCOL_TCG, 0x01);
        for comid in [0x0001u16, 0x1004, 0xFFFF] {
            assert_eq!(cdb_security_in(comid, TCG_ALLOC_LEN).0[1], 0x01);
            assert_eq!(cdb_security_out(comid, 92).0[1], 0x01);
        }

        // 设备回 03/11/00：解锁在 StartSession 即终止，且不下发第二条命令。
        let fake = FakeTransport::new(vec![Err(TransportError::ScsiCheckCondition { sense })]);
        let mut reporter = StepLog::default();
        let mut pwd = Password::new(b"hunter2".to_vec());
        assert_eq!(
            run_unlock(&fake, TEST_COMID, &mut pwd, &mut reporter, &|| false),
            Err(RunError::Protocol(
                ProtocolError::UnsupportedSecurityProtocol {
                    proto: SECURITY_PROTOCOL_TCG,
                    sense,
                }
            ))
        );
        assert_eq!(fake.out_payloads().len(), 1);
        assert_eq!(fake.executed_ins(), 1);
    }

    /// spec §10 锚点：口令写操作返回证据缺口错误，且不下发任何命令。
    #[test]
    fn test_password_write_operations_are_unspecified() {
        let fake = FakeTransport::new(Vec::new());
        assert_eq!(
            set_password(b"new-secret"),
            Err(ProtocolError::PasswordOperationUnspecified)
        );
        assert_eq!(
            delete_password(b"old-secret"),
            Err(ProtocolError::PasswordOperationUnspecified)
        );
        assert!(fake.log.borrow().is_empty(), "不得下发任何命令");
    }

    /// §4.9 口令校验：真口令 → 只发 StartSession + EndSession；假口令 → 只有 StartSession。
    #[test]
    fn test_validate_password_paths() {
        let fake = FakeTransport::new(vec![
            Ok(start_session_response(0)),
            Ok(synthetic_response(TEST_COMID, &hex("fa"))),
        ]);
        let mut pwd = Password::new(b"0123456789abcdef".to_vec());
        assert_eq!(
            run_validate_password(&fake, TEST_COMID, &mut pwd),
            Ok(ValidateOutcome {
                accepted: true,
                status_byte: 0,
            })
        );
        let outs = fake.out_payloads();
        assert_eq!(
            outs.len(),
            2,
            "只发 StartSession 与 EndSession，不发 StartTransaction"
        );
        assert_eq!(tokens_of(&outs[0])[0], 0xF8);
        assert_eq!(tokens_of(&outs[1])[0], 0xFA);

        let rejected = FakeTransport::new(vec![Ok(start_session_response(1))]);
        let mut pwd = Password::new(b"0123456789abcdef".to_vec());
        assert_eq!(
            run_validate_password(&rejected, TEST_COMID, &mut pwd),
            Ok(ValidateOutcome {
                accepted: false,
                status_byte: 1,
            })
        );
        assert_eq!(
            rejected.out_payloads().len(),
            1,
            "被拒时无会话可收尾，不重发命令"
        );
    }

    /// 解锁遇到口令被拒：`SessionRejected`，不推进状态机。
    #[test]
    fn test_unlock_rejects_wrong_password() {
        let fake = FakeTransport::new(vec![Ok(start_session_response(1))]);
        let mut reporter = StepLog::default();
        let mut pwd = Password::new(b"wrong".to_vec());
        assert_eq!(
            run_unlock(&fake, TEST_COMID, &mut pwd, &mut reporter, &|| false),
            Err(RunError::Protocol(ProtocolError::SessionRejected {
                status_byte: 1
            }))
        );
        assert!(reporter.steps().is_empty());
        assert_eq!(fake.out_payloads().len(), 1, "不得进入事务");
    }

    /// `Set` 空应答致命（§4.7/§5）：终止序列并尽力 EndSession。
    #[test]
    fn test_set_empty_response_closes_session() {
        let fake = FakeTransport::new(vec![
            Ok(start_session_response(0)),
            Ok(synthetic_response(TEST_COMID, &hex("fb 00"))),
            Ok(synthetic_response(TEST_COMID, &[])),
            Ok(synthetic_response(TEST_COMID, &hex("fa"))),
        ]);
        let mut reporter = StepLog::default();
        let mut pwd = Password::new(b"0123456789abcdef".to_vec());
        assert_eq!(
            run_unlock(&fake, TEST_COMID, &mut pwd, &mut reporter, &|| false),
            Err(RunError::Protocol(ProtocolError::EmptyResponse {
                step: CommandStep::Unlock(UnlockStep::SetMbrDone),
            }))
        );
        assert_eq!(reporter.steps().len(), 2, "StartTransaction 与第一条 Set");
        let outs = fake.out_payloads();
        assert_eq!(outs.len(), 4, "StartSession + FB + Set#1 + 尽力 EndSession");
        assert_eq!(tokens_of(&outs[3])[0], 0xFA);
    }

    /// 传输失败（设备消失）：原样分类，且清理路径不重放已发过的命令。
    #[test]
    fn test_transport_failure_is_classified_and_not_replayed() {
        let fake = FakeTransport::new(vec![
            Ok(start_session_response(0)),
            Ok(synthetic_response(TEST_COMID, &hex("fb 00"))),
            Err(TransportError::DeviceGone),
            Ok(synthetic_response(TEST_COMID, &hex("fa"))),
        ]);
        let mut reporter = StepLog::default();
        let mut pwd = Password::new(b"0123456789abcdef".to_vec());
        assert_eq!(
            run_unlock(&fake, TEST_COMID, &mut pwd, &mut reporter, &|| false),
            Err(RunError::Transport(TransportError::DeviceGone))
        );
        let outs = fake.out_payloads();
        assert_eq!(outs.len(), 4, "StartSession + FB + Set#1 + 尽力 EndSession");
        assert_eq!(tokens_of(&outs[3])[0], 0xFA);
        assert_eq!(fake.executed_ins(), 4);
    }

    /// §3.2 零值规则：ComID 未取得（0）时不发送任何命令。
    #[test]
    fn test_missing_comid_sends_no_command() {
        let fake = FakeTransport::new(Vec::new());
        let mut reporter = StepLog::default();
        let mut pwd = Password::new(b"hunter2".to_vec());
        assert_eq!(
            run_unlock(&fake, 0, &mut pwd, &mut reporter, &|| false),
            Err(RunError::Protocol(ProtocolError::NoOpalSscDescriptor))
        );
        assert_eq!(
            run_validate_password(&fake, 0, &mut pwd),
            Err(RunError::Protocol(ProtocolError::NoOpalSscDescriptor))
        );
        assert!(fake.log.borrow().is_empty());
        assert!(
            !pwd.is_empty(),
            "未构造报文则不提前清零（由调用方 Drop 兜底）"
        );
    }

    /// §4.2 discovery：只发一次 IN（4096 B 分配、SP specific `0x0001`），结果来自解析。
    #[test]
    fn test_discovery_reads_level0() {
        let fake = FakeTransport::new(vec![Ok(crate::discovery::testkit::level0_fixture())]);
        let discovery = discover(&fake).expect("fixture 必须解析成功");
        let observed_comid: u16 = 0x1004;
        assert_eq!(discovery.base_comid, observed_comid);
        assert_eq!(discovery.locking.raw, 0x1f);
        assert!(discovery.locking.locked());

        let log = fake.log.borrow();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].dir, Direction::In);
        assert_eq!(log[0].data_len, DISCOVERY_ALLOC_LEN as usize);
        assert_eq!(
            log[0].cdb,
            cdb_security_in(DISCOVERY_SP_SPECIFIC, DISCOVERY_ALLOC_LEN)
        );
    }

    /// §4.7：StartSession 应答尾部回显双列表形态 → 本次操作其余帧改用 `Two`。
    #[test]
    fn test_status_list_form_two_is_adopted_for_the_operation() {
        // 37 字节应答体：令牌区 27 字节 + 10 字节双列表（末尾 `F0 00 00 00 F1` 重复两次）。
        let double_body = hex("f8 a8 00 00 00 00 00 00 00 ff a8 00 00 00 00 00 00 ff 02 f0
             01 82 10 1a 80 80 80 f0 00 00 00 f1 f0 00 00 00 f1");
        assert_eq!(double_body.len(), 37);
        let fake = FakeTransport::new(vec![
            Ok(synthetic_response(TEST_COMID, &double_body)),
            Ok(synthetic_response(TEST_COMID, &hex("fb 00"))),
            Ok(set_response(0)),
            Ok(set_response(0)),
            Ok(set_response(0)),
            Ok(set_response(0)),
            Ok(synthetic_response(TEST_COMID, &hex("fc 00"))),
            Ok(synthetic_response(TEST_COMID, &hex("fa"))),
        ]);
        let mut reporter = StepLog::default();
        let mut pwd = Password::new(b"0123456789abcdef".to_vec());
        let session = run_unlock(&fake, TEST_COMID, &mut pwd, &mut reporter, &|| false)
            .expect("解锁必须成功");
        assert_eq!(session.state(), SessionState::Closed);
        assert_eq!(session.ids().hsn, [0x00, 0x00, 0x00, 0x01]);
        assert_eq!(session.ids().tsn, [0x00, 0x00, 0x10, 0x1A]);

        let outs = fake.out_payloads();
        // StartSession 自身固定单列表；其后 7 条命令一律使用双列表形态（总长 68/96）。
        for pkt in &outs[1..] {
            assert!(
                tokens_of(pkt).ends_with(&crate::frame::STATUS_LIST_TWO),
                "双列表形态必须贯穿本次操作"
            );
        }
        assert_eq!(outs[1].len(), 68, "FB 双列表载荷总长");
        assert_eq!(outs[7].len(), 68, "FA 双列表载荷总长");
        assert_eq!(outs[2].len(), 100, "Set 双列表载荷总长");
    }

    /// 取消发生在事务中间：已发出的命令不重放，会话保持打开待调用方收尾。
    #[test]
    fn test_cancel_in_transaction_keeps_session_open() {
        let fake = FakeTransport::new(vec![
            Ok(start_session_response(0)),
            Ok(synthetic_response(TEST_COMID, &hex("fb 00"))),
            Ok(set_response(0)),
            Ok(synthetic_response(TEST_COMID, &hex("fa"))),
        ]);
        let mut reporter = StepLog::default();
        let mut pwd = Password::new(b"0123456789abcdef".to_vec());
        let cancel = || fake.executed_ins() >= 3;
        let mut session =
            run_unlock(&fake, TEST_COMID, &mut pwd, &mut reporter, &cancel).expect("取消不是错误");
        assert_eq!(session.state(), SessionState::InTransaction);
        assert_eq!(
            reporter.steps(),
            vec![UnlockStep::StartTransaction, UnlockStep::SetMbrDone],
            "只上报已执行的步骤"
        );
        assert_eq!(fake.out_payloads().len(), 3);
        session.abort(&fake).expect("尽力 EndSession");
        assert_eq!(session.state(), SessionState::Closed);
        assert_eq!(fake.out_payloads().len(), 4);
    }

    /// 前置条件：会话号为零值（§3.2 零值规则）时拒绝进入事务——`tsn`/`hsn` 任一为零即拒绝。
    #[test]
    fn test_zero_session_ids_block_transactions() {
        // token[4]/token[5] 都解出 0（长度 0 的短原子与全零 8 字节原子），data_len 仍为 37。
        let zero_ids = synthetic_response(
            TEST_COMID,
            &hex("f8 a8 00 00 00 00 00 00 00 ff a8 00 00 00 00 00 00 ff 02 f0
                 80 88 00 00 00 00 00 00 00 00 f1 f9 f0 00 00 00 f1"),
        );
        let fake = FakeTransport::new(vec![Ok(zero_ids)]);
        let mut reporter = StepLog::default();
        let mut pwd = Password::new(b"hunter2".to_vec());
        assert_eq!(
            run_unlock(&fake, TEST_COMID, &mut pwd, &mut reporter, &|| false),
            Err(RunError::Protocol(ProtocolError::SessionIdsMissing))
        );
        assert_eq!(fake.out_payloads().len(), 1, "StartSession 之后再无命令");
        assert!(reporter.steps().is_empty());

        // 只有 TSN 为零（HSN = 1）同样禁止进入事务。
        let tsn_zero = synthetic_response(
            TEST_COMID,
            &hex("f8 a8 00 00 00 00 00 00 00 ff a8 00 00 00 00 00 00 ff 02 f0
                 01 80 80 80 80 80 80 80 80 80 f1 f9 f0 00 00 00 f1"),
        );
        let fake = FakeTransport::new(vec![Ok(tsn_zero)]);
        let mut pwd = Password::new(b"hunter2".to_vec());
        assert_eq!(
            run_unlock(&fake, TEST_COMID, &mut pwd, &mut reporter, &|| false),
            Err(RunError::Protocol(ProtocolError::SessionIdsMissing))
        );
        assert_eq!(fake.out_payloads().len(), 1);
    }
}
