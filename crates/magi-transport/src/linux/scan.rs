//! sysfs 设备扫描与重枚举观察（K3：只用世界可读文件，不提权）。
//!
//! - 扫描：遍历 `/sys/class/scsi_generic/sgN`，从 `device` 链接逐级向上找 `idVendor`/`idProduct`；
//! - 重枚举观察：读 `/sys/block/*`（子分区是否出现）与 `/proc/mounts`（挂载卷）。
//!
//! 所有路径都由调用方以「根目录」形式注入（[`scan_devices_in`] / [`observe_reenumeration_in`]），
//! 因此判定逻辑可以在任意平台上用构造出来的假 sysfs/procfs 树做真测试（真机验证见交付后 V01）。

use std::fs;
use std::path::{Path, PathBuf};

use crate::reenumeration::ReEnumerationSample;
use crate::transport::TransportError;

/// 默认 sysfs 根。
pub const SYSFS_ROOT: &str = "/sys";

/// 默认 procfs 根。
pub const PROCFS_ROOT: &str = "/proc";

/// 设备节点前缀。
const DEV_NODE_PREFIX: &str = "/dev/";

/// scsi_generic 类目录（相对 sysfs 根）。
const SCSI_GENERIC_CLASS: &str = "class/scsi_generic";

/// USB 标识属性文件名。
const KEY_ID_VENDOR: &str = "idVendor";

/// USB 产品号属性文件名。
const KEY_ID_PRODUCT: &str = "idProduct";

/// 一个 SCSI 通用设备节点及其 USB 标识。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxSgDevice {
    /// 设备节点路径（`/dev/sgN`）。
    pub node: String,
    pub vid: u16,
    pub pid: u16,
}

/// 扫描本机 `/sys`，返回 `idVendor == vid` 的全部 SCSI 通用设备节点。
///
/// 只按厂商过滤（PID 判态交给协议层的 `identify_device`），保持单一事实来源。
pub fn scan_devices(vid: u16) -> Result<Vec<LinuxSgDevice>, TransportError> {
    scan_devices_in(Path::new(SYSFS_ROOT), vid)
}

/// [`scan_devices`] 的可注入版本：`sys_root` 为 sysfs 根。
pub fn scan_devices_in(sys_root: &Path, vid: u16) -> Result<Vec<LinuxSgDevice>, TransportError> {
    let class_dir = sys_root.join(SCSI_GENERIC_CLASS);
    let entries = fs::read_dir(&class_dir).map_err(|error| platform(error.raw_os_error()))?;

    let mut devices = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("sg") {
            continue;
        }
        let Some(device_dir) = canonical(&entry.path().join("device")) else {
            continue;
        };
        let (Some(found_vid), Some(found_pid)) = (
            read_hex_u16(&device_dir, KEY_ID_VENDOR),
            read_hex_u16(&device_dir, KEY_ID_PRODUCT),
        ) else {
            continue;
        };
        if found_vid == vid {
            devices.push(LinuxSgDevice {
                node: format!("{DEV_NODE_PREFIX}{name}"),
                vid: found_vid,
                pid: found_pid,
            });
        }
    }
    // 节点顺序稳定，便于调用方与测试引用「第一个设备」。
    devices.sort_by(|left, right| left.node.cmp(&right.node));
    Ok(devices)
}

/// 采集一次重枚举观察采样（真机路径，读 `/sys` 与 `/proc`）。
pub fn observe_reenumeration(vid: u16) -> Result<ReEnumerationSample, TransportError> {
    observe_reenumeration_in(Path::new(SYSFS_ROOT), Path::new(PROCFS_ROOT), vid)
}

/// [`observe_reenumeration`] 的可注入版本。
///
/// `device_present` 取 sysfs 层面的设备节点是否存在；`partition_table_seen` 取该设备对应的块设备
/// 是否出现子分区（锁定态只有影子 FAT16 单卷、无分区表）；`mounted_volumes` 取 `/proc/mounts`
/// 中设备字段属于本设备的挂载点。
pub fn observe_reenumeration_in(
    sys_root: &Path,
    proc_root: &Path,
    vid: u16,
) -> Result<ReEnumerationSample, TransportError> {
    let devices = scan_devices_in(sys_root, vid)?;

    let mut blocks: Vec<String> = Vec::new();
    let mut partitions: Vec<String> = Vec::new();
    for device in &devices {
        let Some(scsi_device) = scsi_device_dir(sys_root, &device.node) else {
            continue;
        };
        for block in block_devices_of(sys_root, &scsi_device) {
            partitions.extend(partitions_of(sys_root, &block));
            blocks.push(block);
        }
    }

    Ok(ReEnumerationSample {
        pid: devices.first().map(|device| device.pid),
        partition_table_seen: !partitions.is_empty(),
        mounted_volumes: mounted_volumes(proc_root, &blocks, &partitions),
        device_present: !devices.is_empty(),
    })
}

/// 设备节点对应的 `scsi_device` 目录（`/sys/class/scsi_generic/sgN/device` 解析后的路径）。
fn scsi_device_dir(sys_root: &Path, node: &str) -> Option<PathBuf> {
    let name = Path::new(node).file_name()?.to_str()?;
    canonical(&sys_root.join(SCSI_GENERIC_CLASS).join(name).join("device"))
}

/// 属于同一 `scsi_device` 的块设备名（`/sys/block/<name>/device` 指向该 `scsi_device`）。
fn block_devices_of(sys_root: &Path, scsi_device: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(sys_root.join("block")) else {
        return Vec::new();
    };
    let mut blocks: Vec<String> = entries
        .flatten()
        .filter(|entry| {
            canonical(&entry.path().join("device")).is_some_and(|target| target == scsi_device)
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    blocks.sort();
    blocks
}

/// 块设备的子分区名（`<块设备名><数字>`，例如 `sda1`）。
fn partitions_of(sys_root: &Path, block: &str) -> Vec<String> {
    let Ok(entries) = fs::read_dir(sys_root.join("block").join(block)) else {
        return Vec::new();
    };
    let mut partitions: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| {
            let Some(index) = name.strip_prefix(block) else {
                return false;
            };
            !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
        })
        .collect();
    partitions.sort();
    partitions
}

/// `/proc/mounts` 中设备字段属于本设备的挂载点。
///
/// 只匹配 `/dev/` 前缀的设备字段（过滤 proc/sysfs 等伪文件系统），不做八进制转义还原。
fn mounted_volumes(proc_root: &Path, blocks: &[String], partitions: &[String]) -> Vec<String> {
    let Ok(content) = fs::read_to_string(proc_root.join("mounts")) else {
        return Vec::new();
    };
    let mut mounts: Vec<String> = content
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let source = fields.next()?;
            let target = fields.next()?;
            let device = source.strip_prefix(DEV_NODE_PREFIX)?;
            let belongs = blocks.iter().any(|block| block == device)
                || partitions.iter().any(|partition| partition == device);
            belongs.then(|| target.to_owned())
        })
        .collect();
    mounts.sort();
    mounts
}

fn read_hex_u16(dir: &Path, key: &str) -> Option<u16> {
    let text = fs::read_to_string(dir.join(key)).ok()?;
    u16::from_str_radix(text.trim(), 16).ok()
}

fn canonical(path: &Path) -> Option<PathBuf> {
    fs::canonicalize(path).ok()
}

fn platform(raw: Option<i32>) -> TransportError {
    TransportError::Platform {
        code: i64::from(raw.unwrap_or(0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const VENDOR: u16 = 0x04E8;

    /// 构造一棵假 sysfs/procfs 树；返回 (sysfs 根, procfs 根)。
    ///
    /// 目录名带进程号与用例名，避免并行测试互相覆盖；重复运行前先清理。
    fn fixture(name: &str) -> (PathBuf, PathBuf) {
        let base =
            std::env::temp_dir().join(format!("magi-transport-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let sys = base.join("sys");
        let proc = base.join("proc");
        fs::create_dir_all(&sys).unwrap();
        fs::create_dir_all(&proc).unwrap();
        (sys, proc)
    }

    /// 写入一个 `sgN` 节点及其 USB 标识（模拟 `scsi_device` 目录树）。
    fn write_sg_device(sys: &Path, name: &str, vid: u16, pid: u16) -> PathBuf {
        let device = sys.join(SCSI_GENERIC_CLASS).join(name).join("device");
        fs::create_dir_all(&device).unwrap();
        fs::write(device.join(KEY_ID_VENDOR), format!("{vid:04x}\n")).unwrap();
        fs::write(device.join(KEY_ID_PRODUCT), format!("{pid:04x}\n")).unwrap();
        device
    }

    /// 为某个 `scsi_device` 挂一个块设备（`/sys/block/<block>/device` → `scsi_device`）。
    fn write_block(sys: &Path, block: &str, scsi_device: &Path) {
        let block_dir = sys.join("block").join(block);
        fs::create_dir_all(&block_dir).unwrap();
        symlink(scsi_device, block_dir.join("device")).unwrap();
    }

    #[test]
    fn test_sysfs_scan_matches_vendor_and_ignores_others() {
        let (sys, _proc) = fixture("scan");
        write_sg_device(&sys, "sg0", VENDOR, 0x61FC);
        write_sg_device(&sys, "sg1", 0x1234, 0x5678);
        // 只读属性缺失的设备（例如非 USB 的 SCSI 设备）必须被忽略，不得 panic。
        fs::create_dir_all(sys.join(SCSI_GENERIC_CLASS).join("sg2").join("device")).unwrap();

        let devices = scan_devices_in(&sys, VENDOR).expect("扫描假 sysfs 树必须成功");
        assert_eq!(
            devices,
            vec![LinuxSgDevice {
                node: "/dev/sg0".to_string(),
                vid: VENDOR,
                pid: 0x61FC,
            }]
        );

        fs::remove_dir_all(sys.parent().unwrap()).unwrap();
    }

    #[test]
    fn test_reenumeration_observation_sees_partitions_and_mounts() {
        let (sys, proc) = fixture("observe");
        let scsi_device = write_sg_device(&sys, "sg0", VENDOR, 0x61FC);
        write_block(&sys, "sda", &scsi_device);
        fs::create_dir_all(sys.join("block/sda/sda1")).unwrap();
        fs::write(
            proc.join("mounts"),
            "proc /proc proc rw 0 0\n/dev/sda1 /run/media/user/T7 vfat rw 0 0\n",
        )
        .unwrap();

        let sample = observe_reenumeration_in(&sys, &proc, VENDOR).expect("观察必须成功");
        assert!(sample.device_present);
        assert_eq!(sample.pid, Some(0x61FC));
        assert!(sample.partition_table_seen, "出现子分区即视为真实分区表");
        assert_eq!(
            sample.mounted_volumes,
            vec!["/run/media/user/T7".to_string()]
        );

        fs::remove_dir_all(sys.parent().unwrap()).unwrap();
    }

    /// 锁定态形态：只有块设备、没有子分区、未挂载 → 判据为「未见分区表」。
    #[test]
    fn test_reenumeration_observation_without_partition_table() {
        let (sys, proc) = fixture("no-partition");
        let scsi_device = write_sg_device(&sys, "sg0", VENDOR, 0x61FC);
        write_block(&sys, "sda", &scsi_device);
        fs::write(proc.join("mounts"), "proc /proc proc rw 0 0\n").unwrap();

        let sample = observe_reenumeration_in(&sys, &proc, VENDOR).expect("观察必须成功");
        assert!(sample.device_present);
        assert!(!sample.partition_table_seen);
        assert!(sample.mounted_volumes.is_empty());

        fs::remove_dir_all(sys.parent().unwrap()).unwrap();
    }

    /// 重枚举期间设备节点消失：采样照常给出，`device_present = false`（窗口内正常现象）。
    #[test]
    fn test_reenumeration_observation_when_device_gone() {
        let (sys, proc) = fixture("gone");
        fs::create_dir_all(sys.join(SCSI_GENERIC_CLASS)).unwrap();
        fs::write(proc.join("mounts"), "proc /proc proc rw 0 0\n").unwrap();

        let sample = observe_reenumeration_in(&sys, &proc, VENDOR).expect("观察必须成功");
        assert!(!sample.device_present);
        assert_eq!(sample.pid, None);
        assert!(!sample.partition_table_seen);
        assert!(sample.mounted_volumes.is_empty());

        fs::remove_dir_all(sys.parent().unwrap()).unwrap();
    }
}
