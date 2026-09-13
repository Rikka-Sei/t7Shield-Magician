//! §4.10 报文头构造与响应解析：`0x38` 字节头、状态列表单/双形态、方法状态字节、
//! `token[i]` 遍历，以及 §4.6/§4.7 的应答长度判据（唯一实现处）。

use crate::atom::{
    walk_atoms, Atom, TOK_ENDLIST, TOK_ENDOFSESSION, TOK_ENDTRANSACTION, TOK_STARTLIST,
    TOK_STARTTRANSACTION,
};
use crate::error::{CommandStep, ProtocolError, UnlockStep};

/// 报文头长度（§4.10）：令牌流从 `+0x38` 起。
pub const PAYLOAD_HEADER: usize = 0x38;

/// §4.6 期望的 StartSession 应答 `data_len`。
pub const START_SESSION_RESPONSE_LEN: usize = 37;
/// §4.7 期望的每条 `Set` 应答 `data_len`。
pub const SET_RESPONSE_LEN: usize = 8;
/// §4.7 期望的 StartTransaction 应答 `data_len`。
pub const START_TRANSACTION_RESPONSE_LEN: usize = 2;
/// §4.7 期望的 EndTransaction 应答 `data_len`。
pub const END_TRANSACTION_RESPONSE_LEN: usize = 2;
/// §4.7 期望的 EndSession 应答 `data_len`。
pub const END_SESSION_RESPONSE_LEN: usize = 1;

/// 单列表状态列表序列 `F0 00 00 00 F1`（§4.10）。
pub const STATUS_LIST_SINGLE: [u8; 5] = [TOK_STARTLIST, 0x00, 0x00, 0x00, TOK_ENDLIST];
/// 双列表状态列表序列：单列表序列重复两次（§4.7 / §4.10）。
pub const STATUS_LIST_TWO: [u8; 10] = [
    TOK_STARTLIST,
    0x00,
    0x00,
    0x00,
    TOK_ENDLIST,
    TOK_STARTLIST,
    0x00,
    0x00,
    0x00,
    TOK_ENDLIST,
];

/// 状态列表形态（§4.7）：选择规则见 §4.10，单次操作内不变。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusListForm {
    /// 5 字节单列表 `F0 00 00 00 F1`。
    Single,
    /// 该序列重复两次，共 10 字节。
    Two,
}

impl StatusListForm {
    /// 该形态的字节序列。
    pub fn as_bytes(self) -> &'static [u8] {
        match self {
            StatusListForm::Single => &STATUS_LIST_SINGLE,
            StatusListForm::Two => &STATUS_LIST_TWO,
        }
    }
}

/// 构造一条 TCG 报文：`0x38` 字节头 + 令牌流，总长按 4 字节对齐补零（§4.10）。
///
/// `tsn` 填 `+0x14`、`hsn` 填 `+0x18`；StartSession 自身报文的两个会话号都是零值。
pub fn make_payload(comid: u16, tsn: [u8; 4], hsn: [u8; 4], tokens: &[u8]) -> Vec<u8> {
    let total = (PAYLOAD_HEADER + tokens.len() + 3) & !3;
    let mut buf = vec![0u8; total];
    buf[0x04..0x06].copy_from_slice(&comid.to_be_bytes());
    buf[0x10..0x14].copy_from_slice(&((total - 0x14) as u32).to_be_bytes());
    buf[0x14..0x18].copy_from_slice(&tsn);
    buf[0x18..0x1c].copy_from_slice(&hsn);
    buf[0x28..0x2c].copy_from_slice(&((total - 0x2c) as u32).to_be_bytes());
    buf[0x34..0x38].copy_from_slice(&(tokens.len() as u32).to_be_bytes());
    buf[PAYLOAD_HEADER..PAYLOAD_HEADER + tokens.len()].copy_from_slice(tokens);
    buf
}

/// 解析出的响应报文（借原始缓冲）。
#[derive(Debug, Clone, Copy)]
pub struct TcgResponse<'a> {
    buf: &'a [u8],
    comid: u16,
    com_packet_len: usize,
    tsn: u32,
    hsn: u32,
    packet_len: usize,
    data_len: usize,
}

/// 解析响应头（§4.10 响应解析表）。
///
/// 缓冲不足 `0x38` 字节，或 `+0x34` 声明的 `data_len` 越过缓冲末尾时返回 `None`
/// （调用方按 §4.3 转成 `TransportError::ShortResponse`，或用 `declared_data_len`
/// 归因 `UnexpectedResponseLength`）。
pub fn parse_response(buf: &[u8]) -> Option<TcgResponse<'_>> {
    if buf.len() < PAYLOAD_HEADER {
        return None;
    }
    let data_len = usize::try_from(be32(&buf[0x34..0x38])).ok()?;
    let body_end = PAYLOAD_HEADER.checked_add(data_len)?;
    if body_end > buf.len() {
        return None;
    }
    Some(TcgResponse {
        buf,
        comid: be16(&buf[0x04..0x06]),
        com_packet_len: usize::try_from(be32(&buf[0x10..0x14])).ok()?,
        tsn: be32(&buf[0x14..0x18]),
        hsn: be32(&buf[0x18..0x1c]),
        packet_len: usize::try_from(be32(&buf[0x28..0x2c])).ok()?,
        data_len,
    })
}

/// `+0x34` 声明的 `data_len`（缓冲不足 `0x38` 时为 `None`）。
pub fn declared_data_len(buf: &[u8]) -> Option<usize> {
    if buf.len() < PAYLOAD_HEADER {
        return None;
    }
    usize::try_from(be32(&buf[0x34..0x38])).ok()
}

/// 判定本次操作的应答状态列表形态（§4.7）：末尾是 10 字节双列表 → `Two`，否则 `Single`。
pub fn detect_status_list_form(resp: &TcgResponse<'_>) -> StatusListForm {
    if resp.data().ends_with(&STATUS_LIST_TWO) {
        StatusListForm::Two
    } else {
        StatusListForm::Single
    }
}

impl<'a> TcgResponse<'a> {
    /// `+0x04`：ComID 回显。
    pub fn comid(&self) -> u16 {
        self.comid
    }

    /// `+0x10`：ComPacket 长度。
    pub fn com_packet_len(&self) -> usize {
        self.com_packet_len
    }

    /// `+0x14`：TSN 回显（原始 32 位值）。
    pub fn tsn(&self) -> u32 {
        self.tsn
    }

    /// `+0x18`：HSN 回显（原始 32 位值）。
    pub fn hsn(&self) -> u32 {
        self.hsn
    }

    /// `+0x28`：Packet 长度。
    pub fn packet_len(&self) -> usize {
        self.packet_len
    }

    /// `+0x34`：`data_len`。
    pub fn data_len(&self) -> usize {
        self.data_len
    }

    /// `+0x38` 起 `data_len` 字节的应答体。
    pub fn data(&self) -> &'a [u8] {
        &self.buf[PAYLOAD_HEADER..PAYLOAD_HEADER + self.data_len]
    }

    /// §4.6/§4.7 的应答长度判据：长度不符即 `UnexpectedResponseLength`。
    pub fn expect_data_len(
        &self,
        expected: usize,
        step: impl Into<CommandStep>,
    ) -> Result<(), ProtocolError> {
        if self.data_len == expected {
            Ok(())
        } else {
            Err(ProtocolError::UnexpectedResponseLength {
                step: step.into(),
                expected,
                actual: self.data_len,
            })
        }
    }

    /// §4.7 的 `Set` 类应答判据：`data_len = 0` 判致命（`EmptyResponse`，会话号错位信号），
    /// 否则长度必须等于 `SET_RESPONSE_LEN`。
    pub fn expect_set_response(&self, step: UnlockStep) -> Result<(), ProtocolError> {
        if self.data_len == 0 {
            return Err(ProtocolError::EmptyResponse { step: step.into() });
        }
        self.expect_data_len(SET_RESPONSE_LEN, step)
    }

    /// §4.10 的方法状态字节规则（四条，唯一实现处）。
    ///
    /// `data_len = 0`（空应答）与形状不符都返回 `None`。
    pub fn status_byte(&self) -> Option<u8> {
        let d = self.data();
        match d.len() {
            0 => None,
            1 if d[0] == TOK_ENDOFSESSION => Some(0),
            2 if d[0] == TOK_STARTTRANSACTION || d[0] == TOK_ENDTRANSACTION => Some(0),
            n if n >= 5 && d[n - 1] == TOK_ENDLIST && d[n - 5] == TOK_STARTLIST => Some(d[n - 4]),
            _ => None,
        }
    }

    /// 取方法状态字节；形状不符（长度对但缺状态列表）时按 `UnexpectedResponseLength`
    /// 归因（§5 没有「缺状态列表」变体，归入应答形状判据）。
    pub fn require_status_byte(
        &self,
        expected: usize,
        step: impl Into<CommandStep>,
    ) -> Result<u8, ProtocolError> {
        self.status_byte()
            .ok_or(ProtocolError::UnexpectedResponseLength {
                step: step.into(),
                expected,
                actual: self.data_len,
            })
    }

    /// 从 `+0x38` 起遍历 `data_len` 字节，在 `data_len - 5` 处停止（末尾是状态列表）。
    pub fn tokens(&self) -> Vec<Atom<'a>> {
        walk_atoms(&self.data()[..self.data_len.saturating_sub(5)])
    }
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// 测试夹具：合成响应帧与十六进制字面量。仅 `cfg(test)` 编译。
#[cfg(test)]
pub(crate) mod testkit {
    use super::PAYLOAD_HEADER;

    /// 十六进制字面量 → 字节（忽略空白；奇数长度视为写错）。
    pub(crate) fn hex(s: &str) -> Vec<u8> {
        let digits: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        assert!(
            digits.len().is_multiple_of(2),
            "十六进制字面量长度必须是偶数：{s}"
        );
        digits
            .chunks_exact(2)
            .map(|pair| {
                let hi = (pair[0] as char).to_digit(16).expect("十六进制字面量");
                let lo = (pair[1] as char).to_digit(16).expect("十六进制字面量");
                (hi * 16 + lo) as u8
            })
            .collect()
    }

    /// 合成一条响应帧：头按 §4.10 头表填充（`+0x14`/`+0x18` 与请求头一样为 0），
    /// 总长按 4 字节对齐。**不**把真机响应帧头写进测试（spec 未规定响应总长取整规则）。
    pub(crate) fn synthetic_response(comid: u16, body: &[u8]) -> Vec<u8> {
        let total = (PAYLOAD_HEADER + body.len() + 3) & !3;
        let mut buf = vec![0u8; total];
        buf[0x04..0x06].copy_from_slice(&comid.to_be_bytes());
        buf[0x10..0x14].copy_from_slice(&((total - 0x14) as u32).to_be_bytes());
        buf[0x28..0x2c].copy_from_slice(&((total - 0x2c) as u32).to_be_bytes());
        buf[0x34..0x38].copy_from_slice(&(body.len() as u32).to_be_bytes());
        buf[PAYLOAD_HEADER..PAYLOAD_HEADER + body.len()].copy_from_slice(body);
        buf
    }

    /// 附录 A4.1 的 37 字节 StartSession 应答体（真口令）。
    pub(crate) fn start_session_body(status_byte: u8) -> Vec<u8> {
        let mut body = hex("f8 a8 00 00 00 00 00 00 00 ff a8 00 00 00 00 00 00 ff 02 f0
             84 00 00 00 01 84 00 00 10 1a  f1 f9");
        body.extend_from_slice(&[0xf0, status_byte, 0x00, 0x00, 0xf1]);
        assert_eq!(body.len(), 37);
        body
    }

    /// StartSession 报文黄金向量（16 字节口令，总计 128 字节）。
    ///
    /// 头按 §4.10 头表逐字段落位：`+0x10 = 0x6c`、`+0x28 = 0x54`、`+0x34 = 0x47`；
    /// 令牌流取自 §4.6 模板（含 5 字节状态列表），尾部 1 字节对齐填充。
    /// 与 `t7Shield-protocol/tools/unlock/unlock.py` 参考实现的输出逐字节一致。
    pub(crate) fn start_session_packet_golden() -> Vec<u8> {
        hex("00 00 00 00 10 04 00 00 00 00 00 00 00 00 00 00
             00 00 00 6c 00 00 00 00 00 00 00 00 00 00 00 00
             00 00 00 00 00 00 00 00 00 00 00 54 00 00 00 00
             00 00 00 00 00 00 00 47 f8 a8 00 00 00 00 00 00
             00 ff a8 00 00 00 00 00 00 ff 02 f0 01 a8 00 00
             02 05 00 00 00 02 01 f2 00 d0 10 30 31 32 33 34
             35 36 37 38 39 61 62 63 64 65 66 f3 f2 03 a8 00
             00 00 09 00 01 00 01 f3 f1 f9 f0 00 00 00 f1 00")
    }

    /// 附录 A1 的令牌流（不含报文头，含尾部状态列表）。
    pub(crate) fn start_session_tokens_golden() -> Vec<u8> {
        start_session_packet_golden()[PAYLOAD_HEADER..0x38 + 0x47].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::{
        hex, start_session_body, start_session_packet_golden, start_session_tokens_golden,
        synthetic_response,
    };
    use super::*;
    use crate::atom::{TOK_CALL, TOK_ENDOFDATA};

    /// §4.10 报文头：总长公式、三个长度域、`+0x04` ComID、`+0x14`/`+0x18` 会话号。
    #[test]
    fn test_make_payload_start_session_golden() {
        let comid: u16 = 0x1004;
        let tokens = start_session_tokens_golden();
        assert_eq!(tokens.len(), 0x47);
        let pkt = make_payload(comid, [0, 0, 0, 0], [0, 0, 0, 0], &tokens);
        assert_eq!(pkt.len(), 128);
        assert_eq!(pkt, start_session_packet_golden());
        assert_eq!(be32(&pkt[0x10..0x14]), (pkt.len() - 0x14) as u32);
        assert_eq!(be32(&pkt[0x28..0x2c]), (pkt.len() - 0x2c) as u32);
        assert_eq!(be32(&pkt[0x34..0x38]), tokens.len() as u32);
    }

    /// 总长按 4 字节对齐：`(0x38 + len + 3) & ~3`，尾部补零。
    #[test]
    fn test_make_payload_aligns_total_length() {
        let pkt = make_payload(
            0x1004,
            [0, 0, 0, 1],
            [0, 0, 0, 2],
            &hex("fb 00 f0 00 00 00 f1"),
        );
        assert_eq!(pkt.len(), 64);
        assert_eq!(&pkt[0x14..0x18], &[0, 0, 0, 1]);
        assert_eq!(&pkt[0x18..0x1c], &[0, 0, 0, 2]);
        assert_eq!(&pkt[0x38..], &hex("fb 00 f0 00 00 00 f1 00"));
    }

    /// §4.10 响应解析：头字段与 `data_len` 越界（`None`）。
    #[test]
    fn test_parse_response_reads_header_and_rejects_overflow() {
        let comid: u16 = 0x1004;
        let buf = synthetic_response(comid, &start_session_body(0));
        let resp = parse_response(&buf).expect("合成响应必须可解析");
        assert_eq!(resp.comid(), comid);
        assert_eq!(resp.data_len(), 37);
        // 合成缓冲按 §4.10 取整：total = (0x38 + 37 + 3) & ~3 = 96。
        assert_eq!(resp.com_packet_len(), 96 - 0x14);
        assert_eq!(resp.packet_len(), 96 - 0x2c);
        assert_eq!(resp.tsn(), 0);
        assert_eq!(resp.hsn(), 0);
        assert_eq!(resp.data(), start_session_body(0));

        assert!(parse_response(&[0u8; PAYLOAD_HEADER - 1]).is_none());
        let mut broken = synthetic_response(comid, &start_session_body(0));
        broken.truncate(0x38 + 36);
        assert!(parse_response(&broken).is_none());
        assert_eq!(declared_data_len(&broken), Some(37));
        assert_eq!(declared_data_len(&[0u8; 4]), None);
    }

    /// §4.10 方法状态字节四条规则 + 空应答 / 形状不符返回 `None`。
    #[test]
    fn test_status_byte_rules() {
        let comid: u16 = 0x1004;
        let ok = synthetic_response(comid, &start_session_body(0));
        assert_eq!(parse_response(&ok).unwrap().status_byte(), Some(0));
        let rejected = synthetic_response(comid, &start_session_body(1));
        assert_eq!(parse_response(&rejected).unwrap().status_byte(), Some(1));

        let start_txn = synthetic_response(comid, &hex("fb 00"));
        assert_eq!(parse_response(&start_txn).unwrap().status_byte(), Some(0));
        let end_txn = synthetic_response(comid, &hex("fc 00"));
        assert_eq!(parse_response(&end_txn).unwrap().status_byte(), Some(0));
        let end_session = synthetic_response(comid, &hex("fa"));
        assert_eq!(parse_response(&end_session).unwrap().status_byte(), Some(0));

        let empty = synthetic_response(comid, &[]);
        assert_eq!(parse_response(&empty).unwrap().status_byte(), None);
        let shape_violation = synthetic_response(comid, &hex("00 00 00 00 00 00 00 00"));
        assert_eq!(
            parse_response(&shape_violation).unwrap().status_byte(),
            None
        );
    }

    /// §4.10 令牌遍历：在 `data_len - 5` 停止，`token[4]`/`token[5]` 是可取数值的短原子。
    #[test]
    fn test_tokens_stop_before_status_list() {
        let resp = synthetic_response(0x1004, &start_session_body(0));
        let resp = parse_response(&resp).unwrap();
        let tokens = resp.tokens();
        assert_eq!(tokens.len(), 8);
        assert_eq!(tokens[0], Atom::Token(TOK_CALL));
        assert_eq!(tokens[3], Atom::Token(TOK_STARTLIST));
        assert_eq!(tokens[6], Atom::Token(TOK_ENDLIST));
        assert_eq!(tokens[7], Atom::Token(TOK_ENDOFDATA));
        assert_eq!(crate::atom::atom_u64(&tokens[4]), Ok(0x0000_0001));
        assert_eq!(crate::atom::atom_u64(&tokens[5]), Ok(0x0000_101A));
    }

    /// §4.7 状态列表形态判定：真机单列表 → `Single`，尾部 10 字节双列表 → `Two`。
    #[test]
    fn test_detect_status_list_form() {
        let single = synthetic_response(0x1004, &start_session_body(0));
        assert_eq!(
            detect_status_list_form(&parse_response(&single).unwrap()),
            StatusListForm::Single
        );

        let mut body = start_session_body(0);
        body.extend_from_slice(&STATUS_LIST_SINGLE);
        let double_buf = synthetic_response(0x1004, &body);
        let double = parse_response(&double_buf).unwrap();
        assert_eq!(detect_status_list_form(&double), StatusListForm::Two);
        assert_eq!(StatusListForm::Single.as_bytes(), &STATUS_LIST_SINGLE);
        assert_eq!(StatusListForm::Two.as_bytes(), &STATUS_LIST_TWO);
    }

    /// §4.6/§4.7 长度判据与 `Set` 空应答致命判据的取法。
    #[test]
    fn test_length_judgements() {
        let resp = synthetic_response(0x1004, &start_session_body(0));
        let resp = parse_response(&resp).unwrap();
        assert_eq!(
            resp.expect_data_len(START_SESSION_RESPONSE_LEN, CommandStep::StartSession),
            Ok(())
        );
        assert_eq!(
            resp.expect_data_len(36, CommandStep::StartSession),
            Err(ProtocolError::UnexpectedResponseLength {
                step: CommandStep::StartSession,
                expected: 36,
                actual: 37,
            })
        );

        let empty = synthetic_response(0x1004, &[]);
        let empty = parse_response(&empty).unwrap();
        assert_eq!(
            empty.expect_set_response(UnlockStep::SetReadLocked),
            Err(ProtocolError::EmptyResponse {
                step: CommandStep::Unlock(UnlockStep::SetReadLocked),
            })
        );
        assert_eq!(
            empty.expect_data_len(START_SESSION_RESPONSE_LEN, CommandStep::StartSession),
            Err(ProtocolError::UnexpectedResponseLength {
                step: CommandStep::StartSession,
                expected: 37,
                actual: 0,
            })
        );
    }
}
