//! 测试支撑：脚本化假传输与黄金应答（仅 `cfg(test)`，不进生产路径）。
//!
//! `t7-app` 的测试需要在**不接触真机**的前提下走通协议层编排（`run_unlock` /
//! `run_validate_password`），因此这里提供一个按方向分发的假 `Transport`：
//! - `Out`（SECURITY PROTOCOL OUT）只记录 CDB 与载荷长度，不写数据；
//! - `In`（SECURITY PROTOCOL IN）按脚本顺序把预置应答写进缓冲。
//!
//! 应答帧用 `t7_protocol::frame::make_payload` 组装（与 §4.10 头表同一实现），体为
//! 附录 A4.1/A4.2 的 37 字节黄金向量；本模块不构造任何下发到设备的令牌流。

use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::Duration;

use t7_protocol::frame::make_payload;
use t7_transport::transport::{DeviceTarget, Direction, ScsiCdb, Transport, TransportError};

/// 附录 A4.1：StartSession 应答的 37 字节 `data_len` 体（`status_byte == 0`）。
pub const START_SESSION_BODY_ACCEPTED: [u8; 37] = [
    0xf8, 0xa8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xa8, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0xff, 0x02, 0xf0, 0x84, 0x00, 0x00, 0x00, 0x01, 0x84, 0x00, 0x00, 0x10, 0x1a, 0xf1, 0xf9,
    0xf0, 0x00, 0x00, 0x00, 0xf1,
];

/// StartSession 应答（口令被拒）：与 [`START_SESSION_BODY_ACCEPTED`] 同形，仅状态字节为 1。
pub fn start_session_body_rejected() -> Vec<u8> {
    let mut body = START_SESSION_BODY_ACCEPTED.to_vec();
    // §4.10：方法状态字节 = `data[data_len - 4]`。
    let index = body.len() - 4;
    body[index] = 0x01;
    body
}

/// 由应答体组装一帧响应：头字段语义与 §4.10 一致（`+0x04` ComID 回显、`+0x34` = `data_len`）。
pub fn response_frame(comid: u16, body: &[u8]) -> Vec<u8> {
    // StartSession 请求头的 TSN/HSN 均为 0，响应按 §4.10 回显该值。
    make_payload(comid, [0, 0, 0, 0], [0, 0, 0, 0], body)
}

/// 一次 `execute` 的记录（用于断言「未下发任何命令」与「单条命令只下发一次」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exchange {
    /// 下发的 12 字节 CDB。
    pub cdb: ScsiCdb,
    /// 数据相方向。
    pub direction: Direction,
}

/// GTK 初始化探针（K6）：可用返回 `true`；不可用打印跳过原因后返回 `false`。
///
/// macOS 上 `gtk::init()` 在非主线程会 panic（gtk4-rs 在 `set_initialized` 里断言
/// `pthread_main_np() != 0`），而 `cargo test` 默认在线程池中运行用例，因此这里把
/// 「无 display」与「不在主线程」两种情况都归入跳过，且不 `#[ignore]`（屏障不会被跳过）。
pub fn gtk_ready(test_name: &str) -> bool {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let probed = std::panic::catch_unwind(gtk4::init);
    std::panic::set_hook(hook);
    match probed {
        Ok(Ok(())) => true,
        Ok(Err(err)) => {
            eprintln!("跳过 {test_name}：无法初始化 GTK（{err}），本用例需要 display");
            false
        }
        Err(_) => {
            eprintln!(
                "跳过 {test_name}：该平台要求 GTK 在进程主线程初始化，而 cargo test 在线程池中运行\
                 本用例（真窗口由 `cargo run -p t7-app` 的冒烟运行验证）"
            );
            false
        }
    }
}

/// 脚本化假传输：`In` 按队列顺序返回预置应答，`Out` 只记录。
#[derive(Default)]
pub struct FakeTransport {
    responses: RefCell<VecDeque<Vec<u8>>>,
    exchanges: RefCell<Vec<Exchange>>,
}

impl FakeTransport {
    /// 按顺序预置 `In` 应答。
    pub fn with_responses(responses: Vec<Vec<u8>>) -> Self {
        FakeTransport {
            responses: RefCell::new(responses.into()),
            exchanges: RefCell::new(Vec::new()),
        }
    }

    /// 本假传输记录到的全部收发。
    pub fn exchanges(&self) -> Vec<Exchange> {
        self.exchanges.borrow().clone()
    }

    /// 已下发的命令条数（OUT 次数）。
    pub fn outbound_count(&self) -> usize {
        self.exchanges
            .borrow()
            .iter()
            .filter(|exchange| exchange.direction == Direction::Out)
            .count()
    }
}

impl Transport for FakeTransport {
    fn open(_target: &DeviceTarget) -> Result<Self, TransportError> {
        Ok(FakeTransport::default())
    }

    fn execute(
        &self,
        cdb: &ScsiCdb,
        dir: Direction,
        data: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, TransportError> {
        self.exchanges.borrow_mut().push(Exchange {
            cdb: *cdb,
            direction: dir,
        });
        match dir {
            Direction::Out => Ok(data.len()),
            Direction::In => {
                let response = self
                    .responses
                    .borrow_mut()
                    .pop_front()
                    .ok_or(TransportError::DeviceGone)?;
                let copied = response.len().min(data.len());
                data[..copied].copy_from_slice(&response[..copied]);
                Ok(copied)
            }
        }
    }
}
