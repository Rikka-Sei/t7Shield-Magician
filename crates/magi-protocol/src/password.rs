//! 口令缓冲：`Password(Zeroizing<Vec<u8>>)` 及其生命周期。
//!
//! §6 安全约束：口令缓冲在报文构造完成后立即 zeroize，并在 `Drop` 时二次清零；口令内容
//! 与长度都不写日志（`Debug` 输出为固定占位串，不含内容与长度）。

use std::fmt;

use zeroize::{Zeroize, Zeroizing};

/// 用户口令明文缓冲（§4.6 的 `HostChallenge` 就是它的字节，不派生、不哈希）。
pub struct Password(Zeroizing<Vec<u8>>);

impl Password {
    /// 接管一段口令字节（长度信息不再单独留存）。
    pub fn new(bytes: Vec<u8>) -> Self {
        Password(Zeroizing::new(bytes))
    }

    /// 口令字节（仅用于构造报文）。
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// 是否为空口令（§4.6：空口令不构造 `HostChallenge` 块；是否可用由应用层裁决）。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 报文构造完成后立即清零。
    pub fn zeroize_now(&mut self) {
        self.0.zeroize();
    }
}

impl Drop for Password {
    fn drop(&mut self) {
        // `Zeroizing` 自身也会清零；这里按 §6 要求显式二次清零。
        self.0.zeroize();
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 清零后口令不再可读：`Zeroize for Vec<u8>` 先清零底层缓冲再置空长度。
    #[test]
    fn test_zeroize_now_clears_bytes() {
        let mut pwd = Password::new(b"hunter2-secret".to_vec());
        assert_eq!(pwd.expose(), b"hunter2-secret");
        assert!(!pwd.is_empty());
        pwd.zeroize_now();
        assert!(pwd.expose().is_empty());
        assert!(pwd.is_empty());
        assert!(Password::new(Vec::new()).is_empty());
    }

    /// `Debug` 不得暴露口令内容或长度。
    #[test]
    fn test_debug_does_not_leak() {
        let pwd = Password::new(b"hunter2".to_vec());
        let text = format!("{pwd:?}");
        assert_eq!(text, "Password(<redacted>)");
        assert!(!text.contains("hunter2"));
        assert!(!text.contains('7'));
    }
}
