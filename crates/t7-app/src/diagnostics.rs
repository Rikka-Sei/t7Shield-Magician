//! 诊断环形缓冲与脱敏导出（§6「合规与保留」「安全」）。
//!
//! 记录纪律（§6 零日志）：
//! - **不记录请求载荷，也不记录它的长度**：StartSession 的载荷长度就是口令长度，载荷内容就是
//!   口令明文；因此请求侧只记 CDB 字节（固定 12 字节，不含口令）；
//! - 只记结构字段：CDB 字节、方向、**响应侧**长度判据、呈现码与 i18n 键；
//! - 口令内容与长度都不进缓冲、不进导出文本。
//!
//! 导出纪律：导出前对每条记录再执行一次 [`redact`]（纵深防御）：十六进制与空白分隔十六进制
//! 形态的字节串一律替换为占位符。缓冲上限 512 条，超出即淘汰最旧记录。

use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};

use rust_i18n::t;
use t7_transport::transport::{Direction, ScsiCdb};

/// 记录级别（诊断文本用的稳定标识，非用户可见文案）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// 正常流程事实。
    Info,
    /// 需要关注但不影响分类。
    Warn,
    /// 失败事实（与呈现码配套）。
    Error,
}

impl Level {
    /// 级别标识（导出文本前缀）。
    pub const fn label(self) -> &'static str {
        match self {
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

/// 一条诊断记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 级别。
    pub level: Level,
    /// 已脱敏的消息文本。
    pub message: String,
}

/// 内存环形缓冲（§6：上限 512 条，超出淘汰最旧）。
#[derive(Debug)]
pub struct DiagnosticsRing {
    capacity: usize,
    entries: Mutex<VecDeque<Entry>>,
}

impl Default for DiagnosticsRing {
    fn default() -> Self {
        Self::new()
    }
}

impl DiagnosticsRing {
    /// §6 的缓冲上限。
    pub const CAPACITY: usize = 512;

    /// 新建缓冲（上限 [`DiagnosticsRing::CAPACITY`]）。
    pub fn new() -> Self {
        DiagnosticsRing {
            capacity: Self::CAPACITY,
            entries: Mutex::new(VecDeque::with_capacity(Self::CAPACITY)),
        }
    }

    /// 记录一条消息（写入前先脱敏）。
    pub fn record(&self, level: Level, message: &str) {
        let entry = Entry {
            level,
            message: redact(message),
        };
        let mut entries = self.entries.lock().unwrap_or_else(|err| err.into_inner());
        if entries.len() == self.capacity {
            entries.pop_front();
        }
        entries.push_back(entry);
    }

    /// 记录一次 SCSI 收发（§6：只记结构字段，请求侧不记长度）。
    ///
    /// CDB 以十进制字节列表记录：它是 §6 允许的「CDB 字节」结构字段，而十六进制形态在导出
    /// 脱敏时会被当作字节串过滤掉（[`redact`]），十进制列表既忠实于「记 CDB 字节」又不与
    /// 口令脱敏规则相互干扰。
    pub fn record_exchange(
        &self,
        cdb: &ScsiCdb,
        direction: Direction,
        response_len: Option<usize>,
    ) {
        let cdb_bytes: Vec<String> = cdb.0.iter().map(|byte| byte.to_string()).collect();
        let cdb_text = format!("[{}]", cdb_bytes.join(","));
        let message = match direction {
            // 请求侧：只记 CDB（12 字节，不含口令），不记载荷与载荷长度（载荷长度即口令长度）。
            Direction::Out => format!("OUT cdb={cdb_text}"),
            // 响应侧：长度判据是 §5 的判定依据，可记。
            Direction::In => match response_len {
                Some(len) => format!("IN cdb={cdb_text} response_len={len}"),
                None => format!("IN cdb={cdb_text}"),
            },
        };
        self.record(Level::Info, &message);
    }

    /// 当前记录条数。
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .len()
    }

    /// 缓冲是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 按时间顺序复制当前记录。
    pub fn entries(&self) -> Vec<Entry> {
        self.entries
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// 导出脱敏文本：导出前对每条记录再执行一次 [`redact`]（§6：导出前再次过滤）。
    pub fn export_redacted(&self) -> String {
        let mut text = String::new();
        text.push_str(&t!("diagnostics.title"));
        text.push('\n');
        text.push_str(&t!("diagnostics.redacted_note"));
        text.push('\n');
        text.push_str(&format!(
            "{}: {}\n",
            t!("diagnostics.entry_count"),
            self.len()
        ));
        for entry in self.entries() {
            text.push_str(entry.level.label());
            text.push(' ');
            text.push_str(&redact(&entry.message));
            text.push('\n');
        }
        text
    }
}

/// 全局诊断缓冲（进程内唯一，内存驻留，不落盘）。
pub fn ring() -> &'static DiagnosticsRing {
    static RING: LazyLock<DiagnosticsRing> = LazyLock::new(DiagnosticsRing::new);
    &RING
}

/// 口令脱敏（§6：导出前再次执行；口径与协议仓库 `Log.redact()` 的字节形态过滤等价）：
/// 十六进制与空白分隔十六进制形态的字节串（≥ 8 字节）替换为占位符。
///
/// 本工具不保留口令明文（否则就等于把口令写进内存诊断结构），因此这里过滤的是**字节形态**：
/// 只要不把载荷写进缓冲（由 [`DiagnosticsRing::record_exchange`] 保证），口令就没有进入
/// 诊断文本的通路；本函数是导出侧的纵深防御。
pub fn redact(text: &str) -> String {
    /// 最小可识别字节串长度：8 字节（16 个十六进制字符）。
    const MIN_HEX_CHARS: usize = 16;
    const PLACEHOLDER: &str = "«redacted»";

    let chars: Vec<char> = text.chars().collect();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        if !chars[index].is_ascii_hexdigit() {
            output.push(chars[index]);
            index += 1;
            continue;
        }
        // 候选串 = 连续的十六进制字符，其间允许单空格分组（`ab cd ef` 形态）。
        let start = index;
        let mut hex_chars = 0usize;
        loop {
            let run_start = index;
            while index < chars.len() && chars[index].is_ascii_hexdigit() {
                hex_chars += 1;
                index += 1;
            }
            // 单字符组不足以构成分组形态（`b=…` 里的 `b` 只是普通词首字母）。
            if index - run_start < 2 {
                break;
            }
            let continues = index + 1 < chars.len()
                && chars[index] == ' '
                && chars[index + 1].is_ascii_hexdigit()
                && hex_run_len(&chars, index + 1) >= 2;
            if !continues {
                break;
            }
            index += 1;
        }
        if hex_chars >= MIN_HEX_CHARS {
            output.push_str(PLACEHOLDER);
        } else {
            output.extend(&chars[start..index]);
        }
    }
    output
}

/// 从 `start` 起连续十六进制字符的个数（用于判断分组形态）。
fn hex_run_len(chars: &[char], start: usize) -> usize {
    let mut length = 0;
    while start + length < chars.len() && chars[start + length].is_ascii_hexdigit() {
        length += 1;
    }
    length
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 上限 512 条，超出淘汰最旧（§6）。
    #[test]
    fn test_ring_capacity_evicts_oldest() {
        let ring = DiagnosticsRing::new();
        for index in 0..DiagnosticsRing::CAPACITY + 3 {
            ring.record(Level::Info, &format!("entry-{index}"));
        }
        assert_eq!(ring.len(), DiagnosticsRing::CAPACITY);
        let entries = ring.entries();
        assert_eq!(entries.first().expect("非空").message, "entry-3");
        assert_eq!(
            entries.last().expect("非空").message,
            format!("entry-{}", DiagnosticsRing::CAPACITY + 2)
        );
    }

    /// 导出前再次过滤：十六进制与空白分隔十六进制形态都不出现在导出文本中。
    #[test]
    fn test_export_redacts_byte_forms() {
        let ring = DiagnosticsRing::new();
        ring.record(Level::Info, "token=0011223344556677");
        ring.record(Level::Info, "token=00 11 22 33 44 55 66 77");
        ring.record(Level::Info, "code=TransportFailure short=0011");
        let export = ring.export_redacted();
        assert!(!export.contains("0011223344556677"));
        assert!(!export.contains("00 11 22 33 44 55 66 77"));
        // 短字节串不误伤：长度不足 8 字节的十六进制片段原样保留。
        assert!(export.contains("code=TransportFailure short=0011"));
        assert_eq!(super::redact("token=0011223344556677"), "token=«redacted»");
        assert_eq!(
            super::redact("a=00 11 22 33 44 55 66 77 b=0011 22"),
            "a=«redacted» b=0011 22"
        );
    }

    /// 请求侧只记 CDB（不含口令），且不记载荷长度；响应侧记长度判据。
    #[test]
    fn test_exchange_records_structure_only() {
        let ring = DiagnosticsRing::new();
        let cdb = ScsiCdb([0xb5, 0x01, 0x10, 0x04, 0, 0, 0, 0, 0, 0x80, 0, 0]);
        ring.record_exchange(&cdb, Direction::Out, None);
        ring.record_exchange(&cdb, Direction::In, Some(37));
        let entries = ring.entries();
        assert_eq!(entries[0].message, "OUT cdb=[181,1,16,4,0,0,0,0,0,128,0,0]");
        assert_eq!(
            entries[1].message,
            "IN cdb=[181,1,16,4,0,0,0,0,0,128,0,0] response_len=37"
        );
        // 请求侧不出现载荷长度（口令长度）。
        assert!(!entries[0].message.contains("response_len"));
        assert!(!entries[0].message.contains("payload"));
    }
}
