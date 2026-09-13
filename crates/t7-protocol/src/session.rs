//! §4.6 解锁会话建立（StartSession）与 §4.9 口令校验（ValidatePassword）。
//!
//! StartSession 的 `HostChallenge` 就是用户口令明文（D02：不派生、不哈希）；口令为空时
//! 整段 `F2 00 … F3` 缺席。StartSession 自身固定使用 `StatusListForm::Single`
//! （其应答用于判定本次操作的形态，见 §4.7）。

use crate::atom::{
    atom_u64, bytes_atom, tiny_uint, uid_atom, TOK_CALL, TOK_ENDLIST, TOK_ENDNAME, TOK_ENDOFDATA,
    TOK_STARTLIST, TOK_STARTNAME,
};
use crate::error::{CommandStep, ProtocolError};
use crate::frame::{make_payload, TcgResponse, START_SESSION_RESPONSE_LEN, STATUS_LIST_SINGLE};
use crate::uid::{Uid, ADMIN1, LOCKINGSP, SMUID, STARTSESSION_METHOD};

/// StartSession 请求（§4.6 契约；`base_comid` 只能来自 Discovery 解析结果）。
pub struct StartSessionRequest<'a> {
    /// Level-0 Discovery 运行时解析出的 ComID。
    pub base_comid: u16,
    /// 用户口令明文（`Password::expose()`）。
    pub host_challenge: &'a [u8],
    /// 固定 1。
    pub host_session_number: u8,
    /// LOCKINGSP。
    pub spid: Uid,
    /// 固定 true。
    pub write: bool,
    /// ADMIN1。
    pub authority: Uid,
}

/// 构造 StartSession 报文（§4.6 令牌流模板逐字节；含 5 字节状态列表，总长按 4 字节对齐）。
pub fn start_session_payload(req: &StartSessionRequest<'_>) -> Vec<u8> {
    let mut tokens = Vec::with_capacity(64 + req.host_challenge.len());
    tokens.push(TOK_CALL);
    tokens.extend_from_slice(&uid_atom(&SMUID));
    tokens.extend_from_slice(&uid_atom(&STARTSESSION_METHOD));
    tokens.push(TOK_STARTLIST);
    tokens.extend_from_slice(&tiny_uint(req.host_session_number));
    tokens.extend_from_slice(&uid_atom(&req.spid));
    tokens.extend_from_slice(&tiny_uint(u8::from(req.write)));
    if !req.host_challenge.is_empty() {
        tokens.extend_from_slice(&[TOK_STARTNAME, 0x00]);
        tokens.extend_from_slice(&bytes_atom(req.host_challenge));
        tokens.extend_from_slice(&[TOK_ENDNAME, TOK_STARTNAME, 0x03]);
        tokens.extend_from_slice(&uid_atom(&req.authority));
        tokens.push(TOK_ENDNAME);
    }
    tokens.extend_from_slice(&[TOK_ENDLIST, TOK_ENDOFDATA]);
    tokens.extend_from_slice(&STATUS_LIST_SINGLE);
    make_payload(req.base_comid, [0, 0, 0, 0], [0, 0, 0, 0], &tokens)
}

/// 口令校验报文（§4.9）：与 `start_session_payload` 同构，差异只在调用方不发
/// StartTransaction，收尾只发 EndSession。
pub fn validate_password_payload(req: &StartSessionRequest<'_>) -> Vec<u8> {
    start_session_payload(req)
}

/// 会话号（§4.6 契约；只承载 TSN/HSN，ComID 与状态列表形态一律独立入参，D23）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionIds {
    /// 设备会话号，填报文头 `+0x14`。
    pub tsn: [u8; 4],
    /// 主机会话号，填报文头 `+0x18`。
    pub hsn: [u8; 4],
}

/// 从 StartSession 应答导出会话号（§4.6 映射表，D04，唯一权威映射）：
/// `token[4]` → HSN、`token[5]` → TSN；64 位原子解码后取低 32 位按大端写 4 字节。
pub fn session_ids_from_response(resp: &TcgResponse<'_>) -> Result<SessionIds, ProtocolError> {
    let tokens = resp.tokens();
    let hsn_atom = tokens.get(4).ok_or(ProtocolError::SessionIdsMissing)?;
    let tsn_atom = tokens.get(5).ok_or(ProtocolError::SessionIdsMissing)?;
    let hsn = atom_u64(hsn_atom)? as u32;
    let tsn = atom_u64(tsn_atom)? as u32;
    Ok(SessionIds {
        tsn: tsn.to_be_bytes(),
        hsn: hsn.to_be_bytes(),
    })
}

/// 口令校验结论（§4.9）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidateOutcome {
    pub accepted: bool,
    pub status_byte: u8,
}

/// 判定口令校验结果（§4.9）：SCSI GOOD 不等于成功，只以方法状态字节判定
/// （`0` = 通过，非 0 = 被拒）。
pub fn validate_outcome(resp: &TcgResponse<'_>) -> Result<ValidateOutcome, ProtocolError> {
    resp.expect_data_len(START_SESSION_RESPONSE_LEN, CommandStep::StartSession)?;
    let status_byte =
        resp.require_status_byte(START_SESSION_RESPONSE_LEN, CommandStep::StartSession)?;
    Ok(ValidateOutcome {
        accepted: status_byte == 0,
        status_byte,
    })
}

/// 构造一次 LOCKINGSP/ADMIN1 的 StartSession 请求（§4.6 的固定参数：
/// HostSessionNumber = 1、SPID = LOCKINGSP、Write = 1、authority = ADMIN1）。
pub fn locking_sp_request<'a>(
    base_comid: u16,
    host_challenge: &'a [u8],
) -> StartSessionRequest<'a> {
    StartSessionRequest {
        base_comid,
        host_challenge,
        host_session_number: 1,
        spid: LOCKINGSP,
        write: true,
        authority: ADMIN1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::Atom;
    use crate::frame::testkit::{hex, start_session_body, synthetic_response};
    use crate::frame::PAYLOAD_HEADER;

    /// spec §10 锚点：StartSession 令牌流黄金向量（128 B 报文逐字节）。
    #[test]
    fn test_start_session_frame_golden() {
        let comid: u16 = 0x1004;
        let password = b"0123456789abcdef";
        let req = locking_sp_request(comid, password);
        let pkt = start_session_payload(&req);

        assert_eq!(pkt.len(), 128);
        assert_eq!(
            pkt,
            crate::frame::testkit::start_session_packet_golden(),
            "16 字节口令必须与附录 A1 的 128 字节黄金向量逐字节一致"
        );
        // 16 字节口令走中字节串 D0 10（不是短字节串 A0 10）。
        assert_eq!(pkt[0x38..0x38 + 2], [0xF8, 0xA8]);
        let tokens = &pkt[PAYLOAD_HEADER..];
        let atom_pos = tokens
            .windows(2)
            .position(|w| w == [TOK_STARTNAME, 0x00].as_slice())
            .expect("HostChallenge 块必须存在");
        assert_eq!(tokens[atom_pos + 2..atom_pos + 4], [0xD0, 0x10]);

        // 3 字节口令：短字节串 A3 + 明文（§4.10 编码表）。
        let short = start_session_payload(&locking_sp_request(comid, b"ABC"));
        let short_tokens = hex(
            "f8 a8 00 00 00 00 00 00 00 ff a8 00 00 00 00 00 00 ff 02 f0 01
             a8 00 00 02 05 00 00 00 02 01 f2 00 a3 41 42 43 f3 f2 03
             a8 00 00 00 09 00 01 00 01 f3 f1 f9 f0 00 00 00 f1",
        );
        assert_eq!(short_tokens.len(), 57);
        assert_eq!(
            &short[PAYLOAD_HEADER..PAYLOAD_HEADER + short_tokens.len()],
            &short_tokens[..]
        );
        assert_eq!(short.len(), 116);

        // 空口令：整段 F2 00 … F3 缺席（D02）。
        let empty = start_session_payload(&locking_sp_request(comid, b""));
        let empty_tokens = hex(
            "f8 a8 00 00 00 00 00 00 00 ff a8 00 00 00 00 00 00 ff 02 f0 01
             a8 00 00 02 05 00 00 00 02 01 f1 f9 f0 00 00 00 f1",
        );
        assert_eq!(empty_tokens.len(), 38);
        assert_eq!(
            &empty[PAYLOAD_HEADER..PAYLOAD_HEADER + empty_tokens.len()],
            &empty_tokens[..]
        );
        assert_eq!(empty.len(), 96);
        assert!(!empty
            .windows(2)
            .any(|w| w == [TOK_STARTNAME, 0x00].as_slice()));
        assert!(!empty
            .windows(2)
            .any(|w| w == [TOK_ENDNAME, TOK_STARTNAME].as_slice()));
    }

    /// 口令校验报文与 StartSession 同构（§4.9）。
    #[test]
    fn test_validate_password_payload_matches_start_session() {
        let comid: u16 = 0x1004;
        let req = locking_sp_request(comid, b"0123456789abcdef");
        assert_eq!(validate_password_payload(&req), start_session_payload(&req));
    }

    /// spec §10 锚点：TSN/HSN 映射（`token[4]` → HSN、`token[5]` → TSN）。
    #[test]
    fn test_session_ids_swap_mapping() {
        let comid: u16 = 0x1004;
        let buf = synthetic_response(comid, &start_session_body(0));
        let ids = session_ids_from_response(&parse(&buf)).expect("会话号必须可导出");
        assert_eq!(ids.tsn, [0x00, 0x00, 0x10, 0x1A]);
        assert_eq!(ids.hsn, [0x00, 0x00, 0x00, 0x01]);

        // 负例夹具：把映射写反的实现会得到相反结果，两者必须不同。
        let swapped_body = hex("f8 a8 00 00 00 00 00 00 00 ff a8 00 00 00 00 00 00 ff 02 f0
             84 00 00 10 1a 84 00 00 00 01 f1 f9 f0 00 00 00 f1");
        let swapped_buf = synthetic_response(comid, &swapped_body);
        let swapped = session_ids_from_response(&parse(&swapped_buf)).expect("会话号必须可导出");
        assert_eq!(swapped.tsn, [0x00, 0x00, 0x00, 0x01]);
        assert_eq!(swapped.hsn, [0x00, 0x00, 0x10, 0x1A]);
        assert_ne!(swapped.tsn, ids.tsn);
        assert_ne!(swapped.hsn, ids.hsn);

        // 8 字节短原子按小端（`rev64`）解释，再取低 32 位按大端写 4 字节。
        let eight_body = hex("f8 a8 00 00 00 00 00 00 00 ff a8 00 00 00 00 00 00 ff 02 f0
             88 01 00 00 00 00 00 00 00 88 1a 10 00 00 00 00 00 00
             f1 f9 f0 00 00 00 f1");
        let eight_buf = synthetic_response(comid, &eight_body);
        let eight =
            session_ids_from_response(&parse(&eight_buf)).expect("8 字节原子必须可导出会话号");
        assert_eq!(eight.hsn, [0x00, 0x00, 0x00, 0x01]);
        assert_eq!(eight.tsn, [0x00, 0x00, 0x10, 0x1A]);
    }

    /// 会话号缺失或类型不符（中/长原子、控制 token）→ `SessionIdsMissing`。
    #[test]
    fn test_session_ids_missing() {
        let short_body = hex("f8 a8 00 00 00 00 00 00 00 ff a8 00 00 00 00 00 00 ff 02 f0 84 00 00 00 01 f1 f9 f0 00 00 00 f1");
        let short_buf = synthetic_response(0x1004, &short_body);
        assert_eq!(
            session_ids_from_response(&parse(&short_buf)),
            Err(ProtocolError::SessionIdsMissing)
        );

        let medium_body = hex("f8 a8 00 00 00 00 00 00 00 ff a8 00 00 00 00 00 00 ff 02 f0
             d0 04 00 00 00 01 d0 04 00 00 10 1a f1 f9 f0 00 00 00 f1");
        let medium_buf = synthetic_response(0x1004, &medium_body);
        assert_eq!(
            session_ids_from_response(&parse(&medium_buf)),
            Err(ProtocolError::SessionIdsMissing)
        );
    }

    /// spec §10 锚点：口令被拒只体现在方法状态字节（`1`），SCSI 仍为 GOOD。
    #[test]
    fn test_password_rejected_status_byte_one() {
        let comid: u16 = 0x1004;
        let rejected_buf = synthetic_response(comid, &start_session_body(1));
        let rejected = parse(&rejected_buf);
        assert_eq!(
            validate_outcome(&rejected),
            Ok(ValidateOutcome {
                accepted: false,
                status_byte: 1,
            })
        );

        let accepted_buf = synthetic_response(comid, &start_session_body(0));
        let accepted = parse(&accepted_buf);
        assert_eq!(
            validate_outcome(&accepted),
            Ok(ValidateOutcome {
                accepted: true,
                status_byte: 0,
            })
        );
    }

    /// 长度不符（非 37）由 §4.6/§5 的长度判据拦下，不进入状态字节判定。
    #[test]
    fn test_validate_outcome_rejects_wrong_length() {
        let mut body = start_session_body(0);
        body.pop();
        let buf = synthetic_response(0x1004, &body);
        let resp = parse(&buf);
        assert_eq!(
            validate_outcome(&resp),
            Err(ProtocolError::UnexpectedResponseLength {
                step: CommandStep::StartSession,
                expected: START_SESSION_RESPONSE_LEN,
                actual: 36,
            })
        );
    }

    /// 令牌遍历的分类与 `token[4]`/`token[5]` 的位置（防止把状态列表当参数）。
    #[test]
    fn test_start_session_response_token_layout() {
        let buf = synthetic_response(0x1004, &start_session_body(0));
        let tokens = parse(&buf).tokens();
        assert_eq!(
            tokens,
            vec![
                Atom::Token(TOK_CALL),
                // A8 + SMUID：8 字节短原子按小端取值（rev64）。
                Atom::Uint(u64::from_le_bytes([0, 0, 0, 0, 0, 0, 0, 0xFF])),
                Atom::Uint(u64::from_le_bytes([0, 0, 0, 0, 0, 0, 0xFF, 0x02])),
                Atom::Token(TOK_STARTLIST),
                Atom::Uint(1),
                Atom::Uint(0x101A),
                Atom::Token(TOK_ENDLIST),
                Atom::Token(TOK_ENDOFDATA),
            ]
        );
    }

    fn parse(buf: &[u8]) -> TcgResponse<'_> {
        crate::frame::parse_response(buf).expect("合成响应必须可解析")
    }
}
