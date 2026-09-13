//! §4.7 解锁事务序列：`FB 00` + 4×`Set` + `FC 00` + `FA` 的载荷构造。
//!
//! 五个 payload 函数显式接收 `base_comid`、`&SessionIds` 与 `StatusListForm`（D23）；
//! `SessionIds` 只承载 TSN/HSN，不含 ComID。4 条 `Set` 的固定参数与顺序只在
//! `unlock_sets()` 里定义一次。

use crate::atom::{
    bytes_atom, tiny_uint, uid_atom, uint, TOK_CALL, TOK_ENDLIST, TOK_ENDNAME, TOK_ENDOFDATA,
    TOK_ENDOFSESSION, TOK_ENDTRANSACTION, TOK_STARTLIST, TOK_STARTNAME, TOK_STARTTRANSACTION,
};
use crate::frame::{make_payload, StatusListForm};
use crate::session::SessionIds;
use crate::uid::{Uid, DATASTORE, LOCKINGRANGE_GLOBAL, MBRCONTROL, SET_METHOD};

/// `Set` 一个 Cell（`SetTable(eUID, eColumn, eToken)`）：目标对象 UID + 列号 + 取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetCell {
    pub object: Uid,
    pub column: u8,
    pub value: u8,
}

/// `Set` 一行（`SetTable(eUID, u64 row, vector)`）：目标对象 UID + 行号 + 取值字节串。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetRow {
    pub object: Uid,
    pub row: u64,
    pub value: Vec<u8>,
}

/// 解锁序列里的一条 `Set`（§4.7 表格的两种形态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockSetOp {
    Cell(SetCell),
    Row(SetRow),
}

/// `FB 00` + 状态列表（事务号 0，tiny uint）。
pub fn start_transaction_payload(
    base_comid: u16,
    ids: &SessionIds,
    form: StatusListForm,
) -> Vec<u8> {
    let mut tokens = vec![TOK_STARTTRANSACTION, 0x00];
    tokens.extend_from_slice(form.as_bytes());
    make_payload(base_comid, ids.tsn, ids.hsn, &tokens)
}

/// 一条 `SetCell`：`F8 A8[目标表] A8[SET] F0 F2 01 F0 F2 <列> <值> F3 F1 F3 F1 F9` + 状态列表。
pub fn set_cell_payload(
    base_comid: u16,
    ids: &SessionIds,
    form: StatusListForm,
    cell: &SetCell,
) -> Vec<u8> {
    let mut tokens = Vec::with_capacity(36);
    tokens.push(TOK_CALL);
    tokens.extend_from_slice(&uid_atom(&cell.object));
    tokens.extend_from_slice(&uid_atom(&SET_METHOD));
    tokens.extend_from_slice(&[
        TOK_STARTLIST,
        TOK_STARTNAME,
        0x01,
        TOK_STARTLIST,
        TOK_STARTNAME,
    ]);
    tokens.extend_from_slice(&tiny_uint(cell.column));
    tokens.extend_from_slice(&tiny_uint(cell.value));
    tokens.extend_from_slice(&[
        TOK_ENDNAME,
        TOK_ENDLIST,
        TOK_ENDNAME,
        TOK_ENDLIST,
        TOK_ENDOFDATA,
    ]);
    tokens.extend_from_slice(form.as_bytes());
    make_payload(base_comid, ids.tsn, ids.hsn, &tokens)
}

/// 一条 `SetRow`：`F8 A8[目标表] A8[SET] F0 F2 00 <行> F3 F2 01 <值> F3 F1 F9` + 状态列表。
pub fn set_row_payload(
    base_comid: u16,
    ids: &SessionIds,
    form: StatusListForm,
    row: &SetRow,
) -> Vec<u8> {
    let mut tokens = Vec::with_capacity(36);
    tokens.push(TOK_CALL);
    tokens.extend_from_slice(&uid_atom(&row.object));
    tokens.extend_from_slice(&uid_atom(&SET_METHOD));
    tokens.extend_from_slice(&[TOK_STARTLIST, TOK_STARTNAME, 0x00]);
    tokens.extend_from_slice(&uint(row.row));
    tokens.extend_from_slice(&[TOK_ENDNAME, TOK_STARTNAME, 0x01]);
    tokens.extend_from_slice(&bytes_atom(&row.value));
    tokens.extend_from_slice(&[TOK_ENDNAME, TOK_ENDLIST, TOK_ENDOFDATA]);
    tokens.extend_from_slice(form.as_bytes());
    make_payload(base_comid, ids.tsn, ids.hsn, &tokens)
}

/// `FC 00` + 状态列表（成功路径 token 为 `0`）。
pub fn end_transaction_payload(base_comid: u16, ids: &SessionIds, form: StatusListForm) -> Vec<u8> {
    let mut tokens = vec![TOK_ENDTRANSACTION, 0x00];
    tokens.extend_from_slice(form.as_bytes());
    make_payload(base_comid, ids.tsn, ids.hsn, &tokens)
}

/// `FA` + 状态列表（状态列表之后不再有字节）。
pub fn end_session_payload(base_comid: u16, ids: &SessionIds, form: StatusListForm) -> Vec<u8> {
    let mut tokens = vec![TOK_ENDOFSESSION];
    tokens.extend_from_slice(form.as_bytes());
    make_payload(base_comid, ids.tsn, ids.hsn, &tokens)
}

/// §4.7 的 4 条 `Set`，顺序不可变（唯一一份顺序表；编排层与测试都引用它）。
pub fn unlock_sets() -> [UnlockSetOp; 4] {
    [
        UnlockSetOp::Cell(SetCell {
            object: MBRCONTROL,
            column: 2,
            value: 1,
        }),
        UnlockSetOp::Cell(SetCell {
            object: LOCKINGRANGE_GLOBAL,
            column: 7,
            value: 0,
        }),
        UnlockSetOp::Cell(SetCell {
            object: LOCKINGRANGE_GLOBAL,
            column: 8,
            value: 0,
        }),
        UnlockSetOp::Row(SetRow {
            object: DATASTORE,
            row: 2,
            value: vec![0x03],
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{CommandStep, ProtocolError, UnlockStep};
    use crate::frame::testkit::{hex, start_session_body, synthetic_response};
    use crate::frame::{
        parse_response, SET_RESPONSE_LEN, START_SESSION_RESPONSE_LEN, STATUS_LIST_SINGLE,
    };
    use crate::session::session_ids_from_response;

    /// 测试用会话号（D03：实测值只作测试局部变量）。
    fn ids() -> SessionIds {
        SessionIds {
            tsn: [0x00, 0x00, 0x10, 0x1A],
            hsn: [0x00, 0x00, 0x00, 0x01],
        }
    }

    /// 令牌流区间（不含 4 字节对齐填充）。
    fn tokens_of(pkt: &[u8], len: usize) -> &[u8] {
        &pkt[crate::frame::PAYLOAD_HEADER..crate::frame::PAYLOAD_HEADER + len]
    }

    /// 载荷末尾的对齐填充必须全为 0（§4.10 总长公式的补零）。
    fn assert_zero_padding(pkt: &[u8], tokens_len: usize) {
        assert!(pkt[crate::frame::PAYLOAD_HEADER + tokens_len..]
            .iter()
            .all(|&b| b == 0));
    }

    /// spec §10 锚点：第 4 条 `Set`（DataStore 行 2 ← `0x03`）的逐字节模板与总长。
    #[test]
    fn test_set_datastore_row_two_frame_golden() {
        let comid: u16 = 0x1004;
        let ids = ids();
        let sets = unlock_sets();
        let UnlockSetOp::Row(row) = &sets[3] else {
            panic!("第 4 条 Set 必须是 SetRow");
        };
        let pkt = set_row_payload(comid, &ids, StatusListForm::Single, row);

        assert_eq!(pkt.len(), 92);
        assert_eq!(&pkt[0x04..0x06], &comid.to_be_bytes());
        assert_eq!(&pkt[0x14..0x18], &ids.tsn);
        assert_eq!(&pkt[0x18..0x1c], &ids.hsn);
        let want = hex("f8 a8 00 00 10 01 00 00 00 00 a8 00 00 00 06 00 00 00 17
                 f0 f2 00 02 f3 f2 01 a1 03 f3 f1 f9 f0 00 00 00 f1");
        assert_eq!(tokens_of(&pkt, want.len()), &want[..]);
        assert_zero_padding(&pkt, want.len());
    }

    /// 三条 `SetCell` 的逐字节模板（§4.7 表格）。
    #[test]
    fn test_set_cell_frames_golden() {
        let comid: u16 = 0x1004;
        let ids = ids();
        let sets = unlock_sets();
        let expected = [
            "f8 a8 00 00 08 03 00 00 00 01 a8 00 00 00 06 00 00 00 17
             f0 f2 01 f0 f2 02 01 f3 f1 f3 f1 f9 f0 00 00 00 f1",
            "f8 a8 00 00 08 02 00 00 00 01 a8 00 00 00 06 00 00 00 17
             f0 f2 01 f0 f2 07 00 f3 f1 f3 f1 f9 f0 00 00 00 f1",
            "f8 a8 00 00 08 02 00 00 00 01 a8 00 00 00 06 00 00 00 17
             f0 f2 01 f0 f2 08 00 f3 f1 f3 f1 f9 f0 00 00 00 f1",
        ];
        for (index, want) in expected.iter().enumerate() {
            let UnlockSetOp::Cell(cell) = &sets[index] else {
                panic!("前 3 条 Set 必须是 SetCell");
            };
            let pkt = set_cell_payload(comid, &ids, StatusListForm::Single, cell);
            assert_eq!(pkt.len(), 92, "Set #{index} 载荷总长必须是 92");
            let want = hex(want);
            assert_eq!(
                tokens_of(&pkt, want.len()),
                &want[..],
                "Set #{index} 令牌流"
            );
            assert_zero_padding(&pkt, want.len());
        }

        // §4.7 的目标对象/列号/取值：InvokingID 是目标表 UID，不是 SMUID。
        assert_eq!(
            sets[0],
            UnlockSetOp::Cell(SetCell {
                object: MBRCONTROL,
                column: 2,
                value: 1
            })
        );
        assert_eq!(
            sets[1],
            UnlockSetOp::Cell(SetCell {
                object: LOCKINGRANGE_GLOBAL,
                column: 7,
                value: 0,
            })
        );
        assert_eq!(
            sets[2],
            UnlockSetOp::Cell(SetCell {
                object: LOCKINGRANGE_GLOBAL,
                column: 8,
                value: 0,
            })
        );
        // Set 类命令的 InvokingID 是目标表 UID（不是 SMUID）：上面第 1 条逐字节向量里
        // `tokens[1..10]` 即 A8[MBRCONTROL]，Session Manager 的 UID 不出现。
        let mbr_pkt = set_cell_payload(
            comid,
            &ids,
            StatusListForm::Single,
            &SetCell {
                object: MBRCONTROL,
                column: 2,
                value: 1,
            },
        );
        let mbr = tokens_of(&mbr_pkt, 36);
        assert_eq!(mbr[1..10], crate::atom::uid_atom(&MBRCONTROL)[..]);
        assert_ne!(mbr[1..10], crate::atom::uid_atom(&crate::uid::SMUID)[..]);
    }

    /// `FB`/`FC`/`FA` 三条收尾命令：令牌流与 64 B 载荷总长。
    #[test]
    fn test_transaction_control_frames_golden() {
        let comid: u16 = 0x1004;
        let ids = ids();
        for (pkt, want) in [
            (
                start_transaction_payload(comid, &ids, StatusListForm::Single),
                &hex("fb 00 f0 00 00 00 f1")[..],
            ),
            (
                end_transaction_payload(comid, &ids, StatusListForm::Single),
                &hex("fc 00 f0 00 00 00 f1")[..],
            ),
            (
                end_session_payload(comid, &ids, StatusListForm::Single),
                &hex("fa f0 00 00 00 f1")[..],
            ),
        ] {
            assert_eq!(pkt.len(), 64);
            assert_eq!(tokens_of(&pkt, want.len()), want);
            assert_zero_padding(&pkt, want.len());
            assert_eq!(&pkt[0x14..0x18], &ids.tsn);
            assert_eq!(&pkt[0x18..0x1c], &ids.hsn);
        }
    }

    /// 双列表形态（`StatusListForm::Two`）的载荷总长与尾部。
    #[test]
    fn test_status_list_form_two_payload() {
        let ids = ids();
        let pkt = end_session_payload(0x1004, &ids, StatusListForm::Two);
        assert_eq!(pkt.len(), 68);
        let want = hex("fa f0 00 00 00 f1 f0 00 00 00 f1");
        assert_eq!(tokens_of(&pkt, want.len()), &want[..]);
        assert_zero_padding(&pkt, want.len());
        assert_eq!(&STATUS_LIST_SINGLE[..], &hex("f0 00 00 00 f1")[..]);
    }

    /// spec §10 锚点：`Set` 类空应答判致命，`FB`/`FC`/`FA` 的空应答不作为失败判据。
    #[test]
    fn test_empty_response_is_fatal() {
        let empty = synthetic_response(0x1004, &[]);
        let empty = parse_response(&empty).expect("空应答帧头仍可解析");
        assert_eq!(empty.data_len(), 0);
        assert_eq!(
            empty.expect_set_response(UnlockStep::SetReadLocked),
            Err(ProtocolError::EmptyResponse {
                step: CommandStep::Unlock(UnlockStep::SetReadLocked),
            })
        );
        // §5：StartTransaction / EndTransaction / EndSession 的空应答不是失败判据，
        // 其成功性由后续判据与 EndSession 的 data_len = 1 交叉验证；空应答也没有状态字节。
        assert_eq!(empty.status_byte(), None);
    }

    /// spec §10 锚点：应答长度不等于期望值（37 / 8 / 2 / 1）时拒绝。
    #[test]
    fn test_unexpected_response_length_rejected() {
        // StartSession：期望 37，实际 36。
        let mut body = start_session_body(0);
        body.pop();
        let short_session_buf = synthetic_response(0x1004, &body);
        let short_session = parse_response(&short_session_buf).expect("帧头可解析");
        assert_eq!(
            short_session.expect_data_len(START_SESSION_RESPONSE_LEN, CommandStep::StartSession),
            Err(ProtocolError::UnexpectedResponseLength {
                step: CommandStep::StartSession,
                expected: START_SESSION_RESPONSE_LEN,
                actual: 36,
            })
        );

        // 第 1 条 Set：期望 8，实际 9。
        let long_set_buf = synthetic_response(0x1004, &hex("f8 00 00 f0 00 00 00 f1 00"));
        let long_set = parse_response(&long_set_buf).expect("帧头可解析");
        assert_eq!(
            long_set.expect_set_response(UnlockStep::SetMbrDone),
            Err(ProtocolError::UnexpectedResponseLength {
                step: CommandStep::Unlock(UnlockStep::SetMbrDone),
                expected: SET_RESPONSE_LEN,
                actual: 9,
            })
        );

        // EndSession：期望 1，实际 2。
        let long_end_buf = synthetic_response(0x1004, &hex("fa 00"));
        let long_end = parse_response(&long_end_buf).expect("帧头可解析");
        assert_eq!(
            long_end.expect_data_len(
                crate::frame::END_SESSION_RESPONSE_LEN,
                UnlockStep::EndSession
            ),
            Err(ProtocolError::UnexpectedResponseLength {
                step: CommandStep::Unlock(UnlockStep::EndSession),
                expected: crate::frame::END_SESSION_RESPONSE_LEN,
                actual: 2,
            })
        );
    }

    /// 会话号来自 StartSession 应答（§4.6），事务内命令的 `+0x14`/`+0x18` 用它填充。
    #[test]
    fn test_session_ids_flow_into_transaction_headers() {
        let resp = synthetic_response(0x1004, &start_session_body(0));
        let ids = session_ids_from_response(&parse_response(&resp).unwrap()).unwrap();
        assert_eq!(ids.tsn, [0x00, 0x00, 0x10, 0x1A]);
        let pkt = start_transaction_payload(0x1004, &ids, StatusListForm::Single);
        assert_eq!(&pkt[0x14..0x18], &[0x00, 0x00, 0x10, 0x1A]);
        assert_eq!(&pkt[0x18..0x1c], &[0x00, 0x00, 0x00, 0x01]);
    }
}
