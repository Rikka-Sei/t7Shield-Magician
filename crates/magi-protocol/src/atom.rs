//! §4.10 原子编码 / 解码。
//!
//! 编码侧只实现 §4.10 编码表列出的形式：token 单字节、tiny uint（≤ `0x3F`）、窄整数
//! （`0x81`–`0x84` + 1/2/4/8 字节 BE）、短/中/长字节串、UID（`0xA8` + 8 字节）。
//!
//! 解码侧照抄 §4.10 的响应解析规则：短原子长度 8 按**小端**解释（官方 `GetUint64` 对
//! `n == 8` 走 `rev64`），其余长度按大端；中/长原子是字节串，没有 64 位数值。

use crate::error::ProtocolError;
use crate::uid::Uid;

/// `F0`：STARTLIST（参数列表起始；状态列表开头）。
pub const TOK_STARTLIST: u8 = 0xF0;
/// `F1`：ENDLIST（参数列表结束；状态列表结尾）。
pub const TOK_ENDLIST: u8 = 0xF1;
/// `F2`：STARTNAME（Cell 名起始）。
pub const TOK_STARTNAME: u8 = 0xF2;
/// `F3`：ENDNAME（Cell 名结束）。
pub const TOK_ENDNAME: u8 = 0xF3;
/// `F8`：CALL（方法调用起始）。
pub const TOK_CALL: u8 = 0xF8;
/// `F9`：ENDOFDATA（调用数据结束）。
pub const TOK_ENDOFDATA: u8 = 0xF9;
/// `FA`：ENDOFSESSION（关闭会话）。
pub const TOK_ENDOFSESSION: u8 = 0xFA;
/// `FB`：STARTTRANSACTION。
pub const TOK_STARTTRANSACTION: u8 = 0xFB;
/// `FC`：ENDTRANSACTION。
pub const TOK_ENDTRANSACTION: u8 = 0xFC;
/// `FF`：EMPTYATOM（省略参数）。
pub const TOK_EMPTYATOM: u8 = 0xFF;

/// tiny uint：值 ≤ `0x3F` 时就是单字节值本身（§4.10）。
pub fn tiny_uint(v: u8) -> Vec<u8> {
    debug_assert!(v <= 0x3F, "tiny uint 只覆盖 ≤ 0x3F，更大值必须走 uint()");
    vec![v]
}

/// 窄整数：按数值宽度选 `0x81`/`0x82`/`0x83`/`0x84` + 1/2/4/8 字节 BE（§4.10）。
pub fn uint(v: u64) -> Vec<u8> {
    if v <= 0x3F {
        return vec![v as u8];
    }
    if v <= 0xFF {
        return vec![0x81, v as u8];
    }
    if v <= 0xFFFF {
        let b = (v as u16).to_be_bytes();
        return vec![0x82, b[0], b[1]];
    }
    if v <= 0xFFFF_FFFF {
        let mut out = Vec::with_capacity(5);
        out.push(0x83);
        out.extend_from_slice(&(v as u32).to_be_bytes());
        return out;
    }
    let mut out = Vec::with_capacity(9);
    out.push(0x84);
    out.extend_from_slice(&v.to_be_bytes());
    out
}

/// 字节串：`len ≤ 0x0F` → 短字节串 `0xA0 | len`；`≤ 0x7FF` → 中字节串
/// `0xD0 | (len >> 8), len & 0xFF`；否则长字节串 `0xE2` + BE32(len)（§4.10）。
pub fn bytes_atom(data: &[u8]) -> Vec<u8> {
    let n = data.len();
    let mut out = Vec::with_capacity(n + 6);
    if n <= 0x0F {
        out.push(0xA0 | n as u8);
    } else if n <= 0x7FF {
        out.push(0xD0 | (n >> 8) as u8);
        out.push((n & 0xFF) as u8);
    } else {
        out.push(0xE2);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    }
    out.extend_from_slice(data);
    out
}

/// UID 原子：`0xA8` + 8 字节（§4.10）。
pub fn uid_atom(u: &Uid) -> Vec<u8> {
    let mut out = Vec::with_capacity(9);
    out.push(0xA8);
    out.extend_from_slice(u);
    out
}

/// 遍历得到的原子（§4.10 解码表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Atom<'a> {
    /// 控制 token（`0xF0`–`0xFF`）。没有数值。
    Token(u8),
    /// 裸 token（值 = 字节 `& 0x3F`）或短原子的数值。
    Uint(u64),
    /// 中/长原子，或长度 > 8 的短原子：字节串，没有 64 位数值。
    Bytes(&'a [u8]),
}

/// 按 §4.10 的解码表遍历原子。
///
/// 调用方通常只传「令牌遍历区间」（`data[..data_len - 5]`，末尾 5 字节是状态列表）。
/// 声明的载荷越过缓冲末尾时（尾部不完整）停止遍历，不把不完整项当作原子——与 §4.2 的
/// 描述符遍历同口径；协议层不为此发明新的错误变体（§5 无对应分类）。
pub fn walk_atoms(buf: &[u8]) -> Vec<Atom<'_>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < buf.len() {
        let b = buf[i];
        if b < 0x80 {
            out.push(Atom::Uint(u64::from(b & 0x3F)));
            i += 1;
        } else if b < 0xC0 {
            let n = usize::from(b & 0x0F);
            let Some(payload) = buf.get(i + 1..i + 1 + n) else {
                break;
            };
            out.push(short_atom_value(payload));
            i += 1 + n;
        } else if b < 0xE0 {
            let Some(hi) = buf.get(i + 1) else { break };
            let n = (usize::from(b & 0x07) << 8) | usize::from(*hi);
            let Some(payload) = buf.get(i + 2..i + 2 + n) else {
                break;
            };
            out.push(Atom::Bytes(payload));
            i += 2 + n;
        } else if b < 0xF0 {
            let Some(len) = buf.get(i + 1..i + 4) else {
                break;
            };
            let n = (usize::from(len[0]) << 16) | (usize::from(len[1]) << 8) | usize::from(len[2]);
            let Some(payload) = buf.get(i + 4..i + 4 + n) else {
                break;
            };
            out.push(Atom::Bytes(payload));
            i += 4 + n;
        } else {
            out.push(Atom::Token(b));
            i += 1;
        }
    }
    out
}

/// 短原子（`0x80`–`0xBF`）的数值：长度 8 小端、其余长度大端、长度 0 为 0；
/// 长度 > 8 无法用 64 位承载，按字节串处理（§4.10 解码表）。
fn short_atom_value(payload: &[u8]) -> Atom<'_> {
    match payload.len() {
        0 => Atom::Uint(0),
        8 => {
            let mut raw = [0u8; 8];
            raw.copy_from_slice(payload);
            Atom::Uint(u64::from_le_bytes(raw))
        }
        1..=7 => {
            let mut v = 0u64;
            for &byte in payload {
                v = (v << 8) | u64::from(byte);
            }
            Atom::Uint(v)
        }
        _ => Atom::Bytes(payload),
    }
}

/// 取原子的 64 位数值（§4.10 数值解码）：控制 token 与字节串没有数值，
/// 出现在需要数值的位置即为 `SessionIdsMissing`。
pub fn atom_u64(a: &Atom<'_>) -> Result<u64, ProtocolError> {
    match a {
        Atom::Uint(v) => Ok(*v),
        Atom::Token(_) | Atom::Bytes(_) => Err(ProtocolError::SessionIdsMissing),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uid::MBRCONTROL;

    /// §4.10 编码表：tiny uint / 窄整数逐项。
    #[test]
    fn test_encode_tiny_and_narrow_uint() {
        assert_eq!(tiny_uint(0), [0x00]);
        assert_eq!(tiny_uint(2), [0x02]);
        assert_eq!(tiny_uint(0x3F), [0x3F]);
        assert_eq!(uint(0), [0x00]);
        assert_eq!(uint(0x40), [0x81, 0x40]);
        assert_eq!(uint(0xFF), [0x81, 0xFF]);
        assert_eq!(uint(0xFFFF), [0x82, 0xFF, 0xFF]);
        assert_eq!(uint(0x10000), [0x83, 0x00, 0x01, 0x00, 0x00]);
        assert_eq!(
            uint(0x1_0000_0000),
            [0x84, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00]
        );
    }

    /// §4.10 编码表：短 / 中 / 长字节串。
    #[test]
    fn test_encode_byte_strings() {
        assert_eq!(bytes_atom(b"ABC"), [0xA3, b'A', b'B', b'C']);
        assert_eq!(bytes_atom(&[b'A'; 16])[..2], [0xD0, 0x10]);
        assert_eq!(bytes_atom(&[b'A'; 32])[..2], [0xD0, 0x20]);
        assert_eq!(bytes_atom(&[b'A'; 0x7FF])[..2], [0xD7, 0xFF]);
        let long = bytes_atom(&[b'A'; 0x800]);
        assert_eq!(long[..5], [0xE2, 0x00, 0x00, 0x08, 0x00]);
        assert_eq!(long.len(), 0x805);
    }

    /// §4.10 编码表：UID 原子。
    #[test]
    fn test_encode_uid_atom() {
        assert_eq!(
            uid_atom(&MBRCONTROL),
            [0xA8, 0x00, 0x00, 0x08, 0x03, 0x00, 0x00, 0x00, 0x01]
        );
    }

    /// §4.10 解码表：裸 token / 短原子 / 中原子 / 长原子 / 控制 token 的分类。
    #[test]
    fn test_walk_atoms_classifies_each_form() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0x41]); // < 0x80 裸 token
        buf.extend_from_slice(&[0x83, 0x00, 0x10, 0x1A]); // 短原子 4 字节
        buf.extend_from_slice(&[0xD0, 0x03, b'x', b'y', b'z']); // 中原子 3 字节
        buf.extend_from_slice(&[0xE2, 0x00, 0x00, 0x02, 0xAA, 0xBB]); // 长原子 2 字节
        buf.extend_from_slice(&[0xF0, 0xF1]); // 控制 token
        assert_eq!(
            walk_atoms(&buf),
            vec![
                Atom::Uint(0x01),
                Atom::Uint(0x0000_101A),
                Atom::Bytes(b"xyz"),
                Atom::Bytes(&[0xAA, 0xBB]),
                Atom::Token(TOK_STARTLIST),
                Atom::Token(TOK_ENDLIST),
            ]
        );
    }

    /// §4.10 数值解码：8 字节短原子按小端（官方 `GetUint64` 的 `rev64` 行为），
    /// 其余长度按大端；长度 > 8 的短原子与中/长原子无数值。
    #[test]
    fn test_decode_short_atom_endianness() {
        let eight = [0x88, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        let atoms = walk_atoms(&eight);
        assert_eq!(atoms.len(), 1);
        assert_eq!(atom_u64(&atoms[0]), Ok(0x8877_6655_4433_2211));

        let four = [0x84, 0x00, 0x00, 0x10, 0x1A];
        assert_eq!(atom_u64(&walk_atoms(&four)[0]), Ok(0x0000_101A));

        let zero = [0x80];
        assert_eq!(atom_u64(&walk_atoms(&zero)[0]), Ok(0));

        let nine = [0x89, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        assert_eq!(
            walk_atoms(&nine)[0],
            Atom::Bytes(&[1, 2, 3, 4, 5, 6, 7, 8, 9])
        );

        let medium = [0xD0, 0x02, 0x00, 0x01];
        assert_eq!(
            atom_u64(&walk_atoms(&medium)[0]),
            Err(ProtocolError::SessionIdsMissing)
        );

        let long = [0xE2, 0x00, 0x00, 0x01, 0x00];
        assert_eq!(
            atom_u64(&walk_atoms(&long)[0]),
            Err(ProtocolError::SessionIdsMissing)
        );
    }

    /// 尾部不完整（声明长度越过缓冲末尾）时停止遍历，不产生半截原子。
    #[test]
    fn test_walk_atoms_stops_on_truncated_tail() {
        // 尾部声明 3 字节却只剩 2 字节：整个不完整项被丢弃（不产生半截原子）。
        assert_eq!(walk_atoms(&[0x83, 0x00, 0x10]), Vec::new());
        assert_eq!(
            walk_atoms(&[0x41, 0xD0, 0x04, 0x00]),
            vec![Atom::Uint(0x01)]
        );
        assert_eq!(walk_atoms(&[0xE2, 0x00, 0x00]), Vec::new());
    }
}
