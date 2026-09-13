//! USB 配置描述符解析：`UsbDescriptorSummary` / `AlternateSetting` / `Endpoint` 与
//! [`parse_config_descriptor`]（跨平台纯函数，无 `cfg` 门控）。
//!
//! 分工（K2）：平台侧只负责「拿到描述符字节」（macOS 走 IOKit 注册表，见 `crate::macos`），
//! 字节 → 结构 的解析全部在本文件，因此 macOS 与 Linux 上跑的是同一份解析与同一份 fixture 测试。
//!
//! 字段来源（§4.5 字段表）：接口/备用设置取 `bInterfaceNumber` / `bAlternateSetting` /
//! `bInterfaceClass` / `bInterfaceSubClass` / `bInterfaceProtocol`；端点取 `bEndpointAddress` /
//! `bmAttributes` / `wMaxPacketSize`。`0x30`（SuperSpeed 端点伴生）与 `0x24`（UAS pipe usage）
//! 等描述符按 `bLength` 步进跳过，不参与解析。

use crate::transport::TransportError;

/// 描述符结构非法时的平台码（唯一取值，不再细分）。
pub const DESCRIPTOR_MALFORMED: i64 = -1;

/// 除最外层配置描述符外，任何描述符的最小合法 `bLength`（`bLength` 与 `bDescriptorType`）。
const DESCRIPTOR_HEADER_LEN: usize = 2;

/// 配置描述符长度（`bLength` 固定 9）。
const CONFIGURATION_DESCRIPTOR_LEN: usize = 9;

/// 接口描述符读取到 `bInterfaceProtocol` 所需的最小长度。
const INTERFACE_DESCRIPTOR_LEN: usize = 9;

/// 端点描述符读取到 `wMaxPacketSize` 所需的最小长度。
const ENDPOINT_DESCRIPTOR_LEN: usize = 7;

/// 描述符类型：配置。
const DESC_CONFIGURATION: u8 = 0x02;

/// 描述符类型：接口。
const DESC_INTERFACE: u8 = 0x04;

/// 描述符类型：端点。
const DESC_ENDPOINT: u8 = 0x05;

/// 端点描述符字段（§4.5 字段表中的 `{地址, 属性, 最大包长}`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// `bEndpointAddress`（含方向位，例如 `0x81` / `0x02`）。
    pub address: u8,
    /// `bmAttributes`（`0x02` = bulk）。
    pub attributes: u8,
    /// `wMaxPacketSize`。
    pub max_packet_size: u16,
}

/// 一个接口的备用设置（§4.5/D19 字段集固定）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlternateSetting {
    /// `bInterfaceNumber`。
    pub interface_number: u8,
    /// `bAlternateSetting`。
    pub alternate_setting: u8,
    /// `bInterfaceClass`（`0x08` = Mass Storage）。
    pub class: u8,
    /// `bInterfaceSubClass`（`0x06` = SCSI）。
    pub subclass: u8,
    /// `bInterfaceProtocol`（`0x50` = BOT，`0x62` = UAS）。
    pub protocol: u8,
    /// 该备用设置下的端点，按描述符出现顺序。
    pub endpoints: Vec<Endpoint>,
}

/// 只读描述符侦察结果（§4.5 契约）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDescriptorSummary {
    pub vid: u16,
    pub pid: u16,
    /// 按描述符出现顺序排列的备用设置（至少 1 项）。
    pub alternate_settings: Vec<AlternateSetting>,
}

/// 解析一个 USB 配置描述符缓冲（`bytes` 为设备描述符之外的原始配置描述符字节）。
///
/// `vid`/`pid` 来自调用方读到的设备描述符（配置描述符本身不携带 VID/PID）。结构非法
/// （空缓冲、`bLength` 非法、步进越界、`wTotalLength` 与实际长度不符、无接口描述符）一律返回
/// `Err(TransportError::Platform { code: DESCRIPTOR_MALFORMED })`。
pub fn parse_config_descriptor(
    vid: u16,
    pid: u16,
    bytes: &[u8],
) -> Result<UsbDescriptorSummary, TransportError> {
    let malformed = || TransportError::Platform {
        code: DESCRIPTOR_MALFORMED,
    };

    if bytes.len() < CONFIGURATION_DESCRIPTOR_LEN
        || usize::from(bytes[0]) != CONFIGURATION_DESCRIPTOR_LEN
        || bytes[1] != DESC_CONFIGURATION
    {
        return Err(malformed());
    }

    let total_length = usize::from(u16::from_le_bytes([bytes[2], bytes[3]]));
    if total_length != bytes.len() {
        return Err(malformed());
    }

    let mut alternate_settings: Vec<AlternateSetting> = Vec::new();
    let mut offset = CONFIGURATION_DESCRIPTOR_LEN;
    while offset < total_length {
        let length = usize::from(bytes[offset]);
        if length < DESCRIPTOR_HEADER_LEN || offset + length > total_length {
            return Err(malformed());
        }
        match bytes[offset + 1] {
            DESC_INTERFACE => {
                if length < INTERFACE_DESCRIPTOR_LEN {
                    return Err(malformed());
                }
                alternate_settings.push(AlternateSetting {
                    interface_number: bytes[offset + 2],
                    alternate_setting: bytes[offset + 3],
                    class: bytes[offset + 5],
                    subclass: bytes[offset + 6],
                    protocol: bytes[offset + 7],
                    endpoints: Vec::new(),
                });
            }
            DESC_ENDPOINT => {
                if length < ENDPOINT_DESCRIPTOR_LEN {
                    return Err(malformed());
                }
                let setting = alternate_settings.last_mut().ok_or_else(malformed)?;
                setting.endpoints.push(Endpoint {
                    address: bytes[offset + 2],
                    attributes: bytes[offset + 3],
                    max_packet_size: u16::from_le_bytes([bytes[offset + 4], bytes[offset + 5]]),
                });
            }
            // `0x30` / `0x24` 等描述符按 bLength 跳过（附录 B 的真实描述符含这两类）。
            _ => {}
        }
        offset += length;
    }

    if alternate_settings.is_empty() {
        return Err(malformed());
    }

    Ok(UsbDescriptorSummary {
        vid,
        pid,
        alternate_settings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 附录 B 的真实描述符（锁定态 T7 Shield 上经 IOKit 只读取得，`wTotalLength = 121`）。
    const REAL_CONFIG_DESCRIPTOR: &str = "\
        09 02 79 00 01 01 00 80 70 \
        09 04 00 00 02 08 06 50 00 \
        07 05 81 02 00 04 00 \
        06 30 0f 00 00 00 \
        07 05 02 02 00 04 00 \
        06 30 0f 00 00 00 \
        09 04 00 01 04 08 06 62 00 \
        07 05 81 02 00 04 00 \
        06 30 0f 05 00 00 \
        04 24 03 00 \
        07 05 02 02 00 04 00 \
        06 30 0f 05 00 00 \
        04 24 04 00 \
        07 05 83 02 00 04 00 \
        06 30 0f 05 00 00 \
        04 24 02 00 \
        07 05 04 02 00 04 00 \
        06 30 00 00 00 00 \
        04 24 01 00";

    fn hex(text: &str) -> Vec<u8> {
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            compact.len().is_multiple_of(2),
            "十六进制字面量长度必须为偶数"
        );
        (0..compact.len() / 2)
            .map(|i| u8::from_str_radix(&compact[i * 2..i * 2 + 2], 16).expect("十六进制字节"))
            .collect()
    }

    fn endpoint(address: u8) -> Endpoint {
        Endpoint {
            address,
            attributes: 0x02,
            max_packet_size: 1024,
        }
    }

    /// 描述符解析黄金测试：附录 B 的真实 121 字节描述符。
    #[test]
    fn test_parse_real_config_descriptor() {
        let bytes = hex(REAL_CONFIG_DESCRIPTOR);
        assert_eq!(bytes.len(), 121);

        let summary =
            parse_config_descriptor(0x04E8, 0x61FC, &bytes).expect("真实描述符必须解析成功");
        assert_eq!(summary.vid, 0x04E8);
        assert_eq!(summary.pid, 0x61FC);
        assert_eq!(
            summary.alternate_settings.len(),
            2,
            "1 个接口的 2 个备用设置"
        );

        let bot = &summary.alternate_settings[0];
        assert_eq!(bot.interface_number, 0);
        assert_eq!(bot.alternate_setting, 0);
        assert_eq!(bot.class, 0x08);
        assert_eq!(bot.subclass, 0x06);
        assert_eq!(bot.protocol, 0x50, "备用设置 0 = BOT");
        assert_eq!(bot.endpoints, vec![endpoint(0x81), endpoint(0x02)]);

        let uas = &summary.alternate_settings[1];
        assert_eq!(uas.interface_number, 0);
        assert_eq!(uas.alternate_setting, 1);
        assert_eq!(uas.class, 0x08);
        assert_eq!(uas.subclass, 0x06);
        assert_eq!(uas.protocol, 0x62, "备用设置 1 = UAS");
        assert_eq!(
            uas.endpoints,
            vec![
                endpoint(0x81),
                endpoint(0x02),
                endpoint(0x83),
                endpoint(0x04)
            ]
        );
    }

    /// 结构非法的缓冲一律 `DESCRIPTOR_MALFORMED`（唯一取值）。
    #[test]
    fn test_parse_rejects_malformed_descriptors() {
        let malformed = Err(TransportError::Platform {
            code: DESCRIPTOR_MALFORMED,
        });

        assert_eq!(parse_config_descriptor(0x04E8, 0x61FC, &[]), malformed);

        // bLength 为 0（最外层配置描述符必须是 9 字节）。
        assert_eq!(
            parse_config_descriptor(0x04E8, 0x61FC, &hex("00 02 09 00 01 01 00 80 70")),
            malformed
        );

        // wTotalLength 与实际长度不符（报文头声称 0x79 却只有 9 字节）。
        assert_eq!(
            parse_config_descriptor(0x04E8, 0x61FC, &hex("09 02 79 00 01 01 00 80 70")),
            malformed
        );

        // 无接口描述符（只有配置头）。
        assert_eq!(
            parse_config_descriptor(0x04E8, 0x61FC, &hex("09 02 09 00 01 01 00 80 70")),
            malformed
        );

        // 步进越界：配置头声称 11 字节，接口描述符自身声明 9 字节。
        assert_eq!(
            parse_config_descriptor(0x04E8, 0x61FC, &hex("09 02 0b 00 01 01 00 80 70 09 04")),
            malformed
        );

        // 内部描述符 bLength 为 0。
        assert_eq!(
            parse_config_descriptor(0x04E8, 0x61FC, &hex("09 02 0b 00 01 01 00 80 70 00 04")),
            malformed
        );

        // 接口描述符自身声明长度不足（< 9）。
        assert_eq!(
            parse_config_descriptor(
                0x04E8,
                0x61FC,
                &hex("09 02 0d 00 01 01 00 80 70 04 04 00 00")
            ),
            malformed
        );
    }
}
