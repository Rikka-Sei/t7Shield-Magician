//! §4.13 线程模型与错误呈现：`AppError` / `AppEvent` 与呈现码表。
//!
//! 呈现码与 `AppError` 变体一一对应（下表逐行照抄 §4.13，不得另起别名）；用户可见文案一律
//! 经 [`message_keys`] / [`reason_key`] / [`advice_key`] 返回的 i18n 键取用（每码一句原因 +
//! 一句建议动作），本文件不内联任何可显示字符串（AC-015）。
//!
//! `Display` 输出的是呈现码本身：它是诊断与日志用的稳定标识（ASCII），不是用户可见文案，
//! 因此可以直接进诊断环形缓冲与导出文本。

use std::fmt;

use magi_protocol::{ProtocolError, RunError, UnlockEvidence, UnlockStep};
use magi_transport::transport::TransportError;

/// 呈现码 `Busy`（§4.13）。
pub const CODE_BUSY: &str = "Busy";
/// 呈现码 `PasswordRejected`（§4.13）。
pub const CODE_PASSWORD_REJECTED: &str = "PasswordRejected";
/// 呈现码 `TransportUnavailable`（§4.13）。
pub const CODE_TRANSPORT_UNAVAILABLE: &str = "TransportUnavailable";
/// 呈现码 `DeviceGone`（§4.13）。
pub const CODE_DEVICE_GONE: &str = "DeviceGone";
/// 呈现码 `CommandTimeout`（§4.13）。
pub const CODE_COMMAND_TIMEOUT: &str = "CommandTimeout";
/// 呈现码 `TransportFailure`（§4.13）。
pub const CODE_TRANSPORT_FAILURE: &str = "TransportFailure";
/// 呈现码 `EmptyResponse`（§4.13）。
pub const CODE_EMPTY_RESPONSE: &str = "EmptyResponse";
/// 呈现码 `PasswordOperationUnspecified`（§4.13）。
pub const CODE_PASSWORD_OPERATION_UNSPECIFIED: &str = "PasswordOperationUnspecified";
/// 呈现码 `ProtocolFailure`（§4.13）。
pub const CODE_PROTOCOL_FAILURE: &str = "ProtocolFailure";
/// 呈现码 `EmptyPassword`（§4.13）。
pub const CODE_EMPTY_PASSWORD: &str = "EmptyPassword";

/// 全部呈现码（与 [`message_keys`] 常量表同序；10 个，§4.13）。
pub const PRESENTATION_CODES: [&str; 10] = [
    CODE_BUSY,
    CODE_PASSWORD_REJECTED,
    CODE_TRANSPORT_UNAVAILABLE,
    CODE_DEVICE_GONE,
    CODE_COMMAND_TIMEOUT,
    CODE_TRANSPORT_FAILURE,
    CODE_EMPTY_RESPONSE,
    CODE_PASSWORD_OPERATION_UNSPECIFIED,
    CODE_PROTOCOL_FAILURE,
    CODE_EMPTY_PASSWORD,
];

/// 呈现码 → `(reason 键, advice 键)`：键名固定为 `<呈现码>.reason` / `<呈现码>.advice`。
///
/// 常量表是键名的唯一来源，[`message_keys`] 只做呈现码到该表的查表；测试断言表与
/// [`PRESENTATION_CODES`] 同序同集，且每个键在两份 locale 资源里都存在。
const MESSAGES: [(&str, &str, &str); 10] = [
    (CODE_BUSY, "Busy.reason", "Busy.advice"),
    (
        CODE_PASSWORD_REJECTED,
        "PasswordRejected.reason",
        "PasswordRejected.advice",
    ),
    (
        CODE_TRANSPORT_UNAVAILABLE,
        "TransportUnavailable.reason",
        "TransportUnavailable.advice",
    ),
    (CODE_DEVICE_GONE, "DeviceGone.reason", "DeviceGone.advice"),
    (
        CODE_COMMAND_TIMEOUT,
        "CommandTimeout.reason",
        "CommandTimeout.advice",
    ),
    (
        CODE_TRANSPORT_FAILURE,
        "TransportFailure.reason",
        "TransportFailure.advice",
    ),
    (
        CODE_EMPTY_RESPONSE,
        "EmptyResponse.reason",
        "EmptyResponse.advice",
    ),
    (
        CODE_PASSWORD_OPERATION_UNSPECIFIED,
        "PasswordOperationUnspecified.reason",
        "PasswordOperationUnspecified.advice",
    ),
    (
        CODE_PROTOCOL_FAILURE,
        "ProtocolFailure.reason",
        "ProtocolFailure.advice",
    ),
    (
        CODE_EMPTY_PASSWORD,
        "EmptyPassword.reason",
        "EmptyPassword.advice",
    ),
];

/// §4.13 应用层错误：与呈现码表一一对应（不新增变体，也不得另起别名）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppError {
    /// 同设备已有在飞操作（§6 单飞；命令队列上限 1，溢出即拒绝，不排队）。
    Busy,
    /// 设备侧方法状态字节非 0：口令被拒（SCSI 状态仍为 GOOD）。
    PasswordRejected,
    /// 传输层错误（§5）。
    Transport(TransportError),
    /// 协议层错误（§5）。
    Protocol(ProtocolError),
    /// 口令输入为空：对话框就地提示，不构造任何报文（§4.12）。
    EmptyPassword,
}

impl From<TransportError> for AppError {
    fn from(err: TransportError) -> Self {
        AppError::Transport(err)
    }
}

impl From<ProtocolError> for AppError {
    fn from(err: ProtocolError) -> Self {
        AppError::Protocol(err)
    }
}

impl From<RunError> for AppError {
    fn from(err: RunError) -> Self {
        match err {
            RunError::Transport(err) => AppError::Transport(err),
            // §5：`SessionRejected` 的唯一消费口径是「口令被拒」（允许用户显式重试），
            // 因此它在应用层不是 `ProtocolFailure` 而是独立变体。
            RunError::Protocol(ProtocolError::SessionRejected { .. }) => AppError::PasswordRejected,
            RunError::Protocol(err) => AppError::Protocol(err),
        }
    }
}

/// `Display` = 呈现码（稳定标识，非用户可见文案；诊断与日志沿用）。
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(presentation_code(self))
    }
}

impl std::error::Error for AppError {}

/// §4.13 应用事件：由工作线程经 channel 投递回主线程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEvent {
    /// 进度：与 §4.7 的 7 条命令一一对应（Discovery 与 StartSession 之后）。
    Progress { step: UnlockStep },
    /// 收尾：`evidence` 为 `None` 表示观察窗口耗尽未取得解锁判据。
    Finished { evidence: Option<UnlockEvidence> },
    /// 失败：错误分类与呈现码。
    Failed { error: AppError },
}

/// 呈现码映射（§4.13 表逐行；`Protocol(_)` 与其余 `Transport(_)` 走兜底行）。
pub fn presentation_code(err: &AppError) -> &'static str {
    match err {
        AppError::Busy => CODE_BUSY,
        AppError::PasswordRejected => CODE_PASSWORD_REJECTED,
        AppError::EmptyPassword => CODE_EMPTY_PASSWORD,
        AppError::Transport(TransportError::Unavailable) => CODE_TRANSPORT_UNAVAILABLE,
        AppError::Transport(TransportError::DeviceGone) => CODE_DEVICE_GONE,
        AppError::Transport(TransportError::Timeout { .. }) => CODE_COMMAND_TIMEOUT,
        AppError::Transport(_) => CODE_TRANSPORT_FAILURE,
        AppError::Protocol(ProtocolError::EmptyResponse { .. }) => CODE_EMPTY_RESPONSE,
        AppError::Protocol(ProtocolError::PasswordOperationUnspecified) => {
            CODE_PASSWORD_OPERATION_UNSPECIFIED
        }
        AppError::Protocol(_) => CODE_PROTOCOL_FAILURE,
    }
}

/// 呈现码对应的 `(reason 键, advice 键)`：UI 一律经这两个键取文案。
pub fn message_keys(err: &AppError) -> (&'static str, &'static str) {
    let code = presentation_code(err);
    MESSAGES
        .iter()
        .find(|(entry, _, _)| *entry == code)
        .map(|(_, reason, advice)| (*reason, *advice))
        .expect("呈现码表缺失该码的文案键")
}

/// 失败原因键（§4.13：一句原因）。
pub fn reason_key(err: &AppError) -> &'static str {
    message_keys(err).0
}

/// 失败建议键（§4.13：一句建议动作）。
pub fn advice_key(err: &AppError) -> &'static str {
    message_keys(err).1
}

/// 呈现码对应的原因键（诊断侧按呈现码取用）。
pub fn reason_key_for_code(code: &str) -> Option<&'static str> {
    MESSAGES
        .iter()
        .find(|(entry, _, _)| *entry == code)
        .map(|(_, reason, _)| *reason)
}

/// 默认语言（§6：默认 `zh-CN`）。
pub const DEFAULT_LOCALE: &str = "zh-CN";

/// 可选语言（§6：提供 `en`）。
pub const FALLBACK_LANGUAGE: &str = "en";

/// 由 `LANG`/`LC_ALL` 一类的标签选择界面语言：`en*` 取 `en`，其余（含未设置）取 `zh-CN`。
///
/// 只有 `zh-CN` 与 `en` 两份资源，因此判定只区分「英文环境」与「其他」。
pub fn locale_for_tag(tag: Option<&str>) -> &'static str {
    match tag {
        Some(tag) if tag.trim().to_ascii_lowercase().starts_with("en") => FALLBACK_LANGUAGE,
        _ => DEFAULT_LOCALE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    use magi_protocol::CommandStep;
    use magi_transport::sense::SenseData;
    use magi_transport::transport::TransportError;

    fn sense() -> SenseData {
        SenseData {
            response_code: 0x70,
            sense_key: 0x03,
            asc: 0x11,
            ascq: 0x00,
        }
    }

    /// §4.13 呈现码表逐行断言（含 `Platform`/`PermissionDenied`/`ShortResponse`/
    /// `ScsiCheckCondition` 四行都落到 `TransportFailure`）。
    #[test]
    fn test_presentation_codes_match_spec_table() {
        let step = CommandStep::Unlock(UnlockStep::SetReadLocked);
        let cases: [(AppError, &str); 10] = [
            (AppError::Busy, "Busy"),
            (AppError::PasswordRejected, "PasswordRejected"),
            (
                AppError::Transport(TransportError::Unavailable),
                "TransportUnavailable",
            ),
            (
                AppError::Transport(TransportError::DeviceGone),
                "DeviceGone",
            ),
            (
                AppError::Transport(TransportError::Timeout {
                    elapsed: std::time::Duration::from_secs(30),
                }),
                "CommandTimeout",
            ),
            (
                AppError::Transport(TransportError::Platform { code: -1 }),
                "TransportFailure",
            ),
            (
                AppError::Transport(TransportError::PermissionDenied),
                "TransportFailure",
            ),
            (
                AppError::Transport(TransportError::ShortResponse { got: 16 }),
                "TransportFailure",
            ),
            (
                AppError::Transport(TransportError::ScsiCheckCondition { sense: sense() }),
                "TransportFailure",
            ),
            (
                AppError::Protocol(ProtocolError::EmptyResponse { step }),
                "EmptyResponse",
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(presentation_code(&err), expected, "呈现码表不符：{err:?}");
        }
        assert_eq!(
            presentation_code(&AppError::Protocol(
                ProtocolError::PasswordOperationUnspecified
            )),
            "PasswordOperationUnspecified"
        );
        assert_eq!(
            presentation_code(&AppError::Protocol(ProtocolError::SessionIdsMissing)),
            "ProtocolFailure"
        );
        assert_eq!(presentation_code(&AppError::EmptyPassword), "EmptyPassword");
    }

    /// 文案键表与呈现码集合同序同集，且键名形如 `<呈现码>.reason` / `.advice`。
    #[test]
    fn test_message_keys_cover_every_presentation_code() {
        assert_eq!(MESSAGES.len(), PRESENTATION_CODES.len());
        for (index, code) in PRESENTATION_CODES.iter().enumerate() {
            let (entry, reason, advice) = MESSAGES[index];
            assert_eq!(entry, *code);
            assert_eq!(reason, format!("{code}.reason"));
            assert_eq!(advice, format!("{code}.advice"));
            assert!(reason_key_for_code(code).is_some());
        }
        assert!(reason_key_for_code("NoSuchCode").is_none());
        assert_eq!(
            message_keys(&AppError::Busy),
            ("Busy.reason", "Busy.advice")
        );
        assert_eq!(
            message_keys(&AppError::Transport(TransportError::DeviceGone)),
            ("DeviceGone.reason", "DeviceGone.advice")
        );
    }

    /// `zh-CN` 与 `en` 的键集合完全相等（§6：两份资源齐备，AC-015）。
    #[test]
    fn test_locale_key_sets_are_equal() {
        let zh = flatten_keys(include_str!("../locales/zh-CN.yml"));
        let en = flatten_keys(include_str!("../locales/en.yml"));
        assert!(!zh.is_empty());
        let missing_in_en: Vec<&String> = zh.difference(&en).collect();
        let missing_in_zh: Vec<&String> = en.difference(&zh).collect();
        assert!(missing_in_en.is_empty(), "en 缺键：{missing_in_en:?}");
        assert!(missing_in_zh.is_empty(), "zh-CN 缺键：{missing_in_zh:?}");
    }

    /// 表里每个键在两份资源里都能取到非键名文本（防止「键写错」被静默回退掩盖）。
    #[test]
    fn test_message_keys_resolve_in_both_locales() {
        for (_, reason, advice) in MESSAGES {
            for locale in [DEFAULT_LOCALE, FALLBACK_LANGUAGE] {
                for key in [reason, advice] {
                    let text = rust_i18n::t!(key, locale = locale).to_string();
                    assert_ne!(text, key, "{locale} 缺少 {key}");
                    assert!(!text.trim().is_empty(), "{locale} 的 {key} 为空");
                }
            }
        }
    }

    /// 语言标签选择：`en*` → `en`，其余 → `zh-CN`。
    #[test]
    fn test_locale_selection() {
        assert_eq!(locale_for_tag(Some("en_US.UTF-8")), "en");
        assert_eq!(locale_for_tag(Some("en")), "en");
        assert_eq!(locale_for_tag(Some("zh_CN.UTF-8")), "zh-CN");
        assert_eq!(locale_for_tag(Some("de_DE.UTF-8")), "zh-CN");
        assert_eq!(locale_for_tag(None), "zh-CN");
    }

    /// 极简 YAML 键压平：只处理本项目 locale 文件的形态（嵌套映射 + 引号标量，无列表、
    /// 无多行标量、无行内注释），返回「点分扁平键」集合。
    fn flatten_keys(yaml: &str) -> BTreeSet<String> {
        let mut stack: Vec<(usize, String)> = Vec::new();
        let mut keys = BTreeSet::new();
        for raw in yaml.lines() {
            let line = raw.trim_end();
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let indent = line.len() - line.trim_start().len();
            let (key, value) = line
                .trim_start()
                .split_once(':')
                .unwrap_or_else(|| panic!("locale 行缺少 ':'：{raw}"));
            while matches!(stack.last(), Some((level, _)) if *level >= indent) {
                stack.pop();
            }
            let mut path: Vec<&str> = stack.iter().map(|(_, key)| key.as_str()).collect();
            path.push(key);
            if value.trim().is_empty() {
                stack.push((indent, key.to_string()));
            } else {
                keys.insert(path.join("."));
            }
        }
        keys
    }
}
