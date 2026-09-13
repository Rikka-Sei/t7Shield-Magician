//! IOKit 注册表 + `IOUSBDeviceInterface` 插件的只读 FFI。
//!
//! 本文件只做两件事：按 VID/PID 在注册表里定位 `IOUSBHostDevice`，再经 `IOUSBDeviceInterface`
//! （`kIOUSBDeviceInterfaceID100`）的 `GetConfigurationDescriptorPtr` 取回配置描述符字节。
//! Apple 头文件对该函数原文（`IOUSBLib.h:1151-1161`）：*"The device does not have to be open
//! to use this function."* —— 因此这里**不打开设备、不 claim、不 seize 接口、不发任何 CDB**；
//! 接口被系统驱动（`IOUSBMassStorageInterfaceNub`）独占不影响本路径。
//!
//! ## 句柄的两层结构（本文件最容易写错的地方，已实测核对）
//!
//! `IOCreatePlugInInterfaceForService` 与 `QueryInterface` 写入调用方单元的**不是**接口结构地址，
//! 而是 IOKit 分配的对象指针（句柄）：`*handle` 才是接口结构（方法指针所在），而方法调用的
//! `thisPointer` 必须是**句柄本身**。这正是 C 惯用法 `(*handle)->Method(handle, …)` 的含义：
//! 两个 `handle` 一个用来取方法、一个作为 `this`。把接口结构当句柄传会立刻 SIGSEGV（本机实测）。
//!
//! 结构体槽位来自 SDK 头文件，并用 `offsetof` 实测核对（`IOUSBDeviceStruct100` 的
//! `GetConfigurationDescriptorPtr` 位于第 21 个指针槽位，即偏移 168）；`#[cfg(test)]` 的布局
//! 测试把这一核对固化下来：槽位算错会调到别的设备方法，是必须被拦住的错误。

use std::ffi::{c_char, c_void, CStr};
use std::ptr;

use core_foundation::base::{CFType, TCFType};
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation::uuid::{
    CFUUIDBytes, CFUUIDGetConstantUUIDWithBytes, CFUUIDGetUUIDBytes, CFUUIDRef,
};
use io_kit_sys::ret::{kIOReturnBadArgument, kIOReturnError, kIOReturnNoMemory};
use io_kit_sys::types::{io_iterator_t, io_object_t, io_registry_entry_t, io_service_t};
use io_kit_sys::{
    IOIteratorNext, IOObjectRelease, IORegistryEntryCreateCFProperty, IOServiceGetMatchingServices,
    IOServiceMatching,
};

use crate::transport::TransportError;

/// `KERN_SUCCESS`。
const KERN_SUCCESS: KernReturn = 0;

/// `kIOMainPortDefault`：默认主端口（`MACH_PORT_NULL`），无需再调用已弃用的 `IOMasterPort`。
const K_IO_MAIN_PORT_DEFAULT: u32 = 0;

/// USB 设备节点类名。
const IOUSB_HOST_DEVICE_CLASS: &CStr = c"IOUSBHostDevice";

/// 注册表属性键：`idVendor`。
const KEY_ID_VENDOR: &str = "idVendor";

/// 注册表属性键：`idProduct`。
const KEY_ID_PRODUCT: &str = "idProduct";

/// `kIOUSBDeviceUserClientTypeID`：`9DC7B780-9EC0-11D4-A54F-000A27052861`。
const IOUSB_DEVICE_USER_CLIENT_TYPE_ID: [u8; 16] = [
    0x9d, 0xc7, 0xb7, 0x80, 0x9e, 0xc0, 0x11, 0xd4, 0xa5, 0x4f, 0x00, 0x0a, 0x27, 0x05, 0x28, 0x61,
];

/// `kIOCFPlugInInterfaceID`：`C244E858-109C-11D4-91D4-0050E4C6426F`。
const IOCF_PLUGIN_INTERFACE_ID: [u8; 16] = [
    0xc2, 0x44, 0xe8, 0x58, 0x10, 0x9c, 0x11, 0xd4, 0x91, 0xd4, 0x00, 0x50, 0xe4, 0xc6, 0x42, 0x6f,
];

/// `kIOUSBDeviceInterfaceID100`（= `kIOUSBDeviceInterfaceID`）：`5C8187D0-9EF3-11D4-8B45-000A27052861`。
const IOUSB_DEVICE_INTERFACE_ID100: [u8; 16] = [
    0x5c, 0x81, 0x87, 0xd0, 0x9e, 0xf3, 0x11, 0xd4, 0x8b, 0x45, 0x00, 0x0a, 0x27, 0x05, 0x28, 0x61,
];

/// `IOUSBDeviceStruct100` 中 `GetConfigurationDescriptorPtr` 之前的指针槽数（实测偏移 168 / 8）。
const GET_CONFIGURATION_DESCRIPTOR_PTR_SLOT: usize = 21;

type KernReturn = i32;
type Hresult = i32;

/// 一条匹配设备的只读侦察结果：注册表读到的 VID/PID + 配置描述符原始字节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawDeviceDescriptor {
    pub vid: u16,
    pub pid: u16,
    pub config_descriptor: Vec<u8>,
}

/// `IUnknown` 的 COM 前导（`IUNKNOWN_C_GUTS`，`CFPlugInCOM.h`）。
#[repr(C)]
struct IUnknownGuts {
    _reserved: *mut c_void,
    query_interface:
        Option<unsafe extern "C" fn(*mut c_void, CFUUIDBytes, *mut *mut c_void) -> Hresult>,
    _add_ref: Option<unsafe extern "C" fn(*mut c_void) -> u32>,
    release: Option<unsafe extern "C" fn(*mut c_void) -> u32>,
}

/// `IOCFPlugInInterface`（`IOCFPlugIn.h`：`IUNKNOWN_C_GUTS` + `IOCFPLUGINBASE`）。
#[repr(C)]
struct IocfPlugInInterface {
    guts: IUnknownGuts,
    _version: u16,
    _revision: u16,
    _probe: *mut c_void,
    _start: *mut c_void,
    _stop: *mut c_void,
}

/// `IOUSBDeviceStruct100`（= `IOUSBDeviceInterface`，`IOUSBLib.h:955`）。
///
/// 槽位 4–20 是事件源/端口与设备属性查询方法，本实现一律以不透明指针占位、**从不调用**
/// （尤其不调用任何打开设备或接口、会与系统驱动争用接口的占用型方法）；槽位 21 是只读的
/// `GetConfigurationDescriptorPtr`。
#[repr(C)]
struct IOUsbDeviceInterface {
    guts: IUnknownGuts,
    _opaque_slots: [*mut c_void; GET_CONFIGURATION_DESCRIPTOR_PTR_SLOT - 4],
    get_configuration_descriptor_ptr:
        Option<unsafe extern "C" fn(*mut c_void, u8, *mut *const c_void) -> KernReturn>,
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    /// 由 `io_service_t` 创建插件对象（只创建用户客户端代理，不打开设备）。
    fn IOCreatePlugInInterfaceForService(
        service: io_service_t,
        plugin_type: CFUUIDRef,
        interface_type: CFUUIDRef,
        the_interface: *mut *mut *mut IocfPlugInInterface,
        the_score: *mut i32,
    ) -> KernReturn;
}

/// 只读侦察所有 VID/PID 匹配的 `IOUSBHostDevice`，返回其配置描述符原始字节。
///
/// 逐级降级：找不到匹配服务 → 空 `Vec`；IOKit 调用失败 → `Platform { code }`（不 panic）。
pub fn read_config_descriptors(
    vid: u16,
    pid: u16,
) -> Result<Vec<RawDeviceDescriptor>, TransportError> {
    // SAFETY: `IOServiceMatching` 接受以 NUL 结尾的静态 C 字符串，返回的字典由
    // `IOServiceGetMatchingServices` 消费；以下每个 IOKit 对象的生命周期都在本函数内闭合。
    unsafe {
        let matching = IOServiceMatching(IOUSB_HOST_DEVICE_CLASS.as_ptr() as *const c_char);
        if matching.is_null() {
            return Err(platform(kIOReturnNoMemory));
        }

        let mut iterator: io_iterator_t = 0;
        let kr = IOServiceGetMatchingServices(K_IO_MAIN_PORT_DEFAULT, matching, &mut iterator);
        if kr != KERN_SUCCESS {
            return Err(platform(kr));
        }

        let mut found = Vec::new();
        loop {
            let service: io_object_t = IOIteratorNext(iterator);
            if service == 0 {
                break;
            }
            let probed = probe_service(service, vid, pid);
            IOObjectRelease(service);
            if let Some(descriptor) = probed? {
                found.push(descriptor);
            }
        }
        IOObjectRelease(iterator);

        Ok(found)
    }
}

/// 检查单个服务是否匹配 VID/PID；匹配则取回其配置描述符字节。
///
/// # Safety
/// `service` 必须是调用方持有的有效 `io_service_t`。
unsafe fn probe_service(
    service: io_service_t,
    vid: u16,
    pid: u16,
) -> Result<Option<RawDeviceDescriptor>, TransportError> {
    // SAFETY: service 有效；注册表只读属性，返回值按 create rule 交给 CFType 释放。
    unsafe {
        match registry_u16(service, KEY_ID_VENDOR) {
            Some(device_vid) if device_vid == vid => {}
            _ => return Ok(None),
        }
        match registry_u16(service, KEY_ID_PRODUCT) {
            Some(device_pid) if device_pid == pid => {}
            _ => return Ok(None),
        }

        let mut score: i32 = 0;
        let mut handle: *mut *mut IocfPlugInInterface = ptr::null_mut();
        let kr = IOCreatePlugInInterfaceForService(
            service,
            constant_uuid(IOUSB_DEVICE_USER_CLIENT_TYPE_ID),
            constant_uuid(IOCF_PLUGIN_INTERFACE_ID),
            &mut handle,
            &mut score,
        );
        if kr != KERN_SUCCESS {
            return Err(platform(kr));
        }
        if handle.is_null() {
            return Err(platform(kIOReturnError));
        }

        // RAII：插件对象与设备对象都在 Drop 中 Release，不留引用。
        let plugin = PlugInInterface(handle);
        let device = plugin.query_device_interface()?;
        let config_descriptor = device.configuration_descriptor(0)?;

        Ok(Some(RawDeviceDescriptor {
            vid,
            pid,
            config_descriptor,
        }))
    }
}

/// 读取注册表整数属性（`idVendor` / `idProduct`）；缺失或不是 CFNumber 时返回 `None`。
///
/// # Safety
/// `entry` 必须是有效的注册表项。
unsafe fn registry_u16(entry: io_registry_entry_t, key: &str) -> Option<u16> {
    let cf_key = CFString::new(key);
    // SAFETY: 键为 CFString；返回值遵循 create rule，交由 CFType 管理引用计数。
    let value = unsafe {
        IORegistryEntryCreateCFProperty(entry, cf_key.as_concrete_TypeRef(), ptr::null(), 0)
    };
    if value.is_null() {
        return None;
    }
    // SAFETY: 非空返回值由本函数持有（create rule），downcast_into 接管所有权。
    let number: CFNumber = unsafe { CFType::wrap_under_create_rule(value) }.downcast_into()?;
    number.to_i64().map(|raw| raw as u16)
}

/// `CFUUIDGetConstantUUIDWithBytes` 的封装（`kCFAllocatorDefault` + 16 个 UUID 字节）。
///
/// 该函数对相同字节返回同一个常量对象，因此这里构造的 UUID 与 IOUSBLib 内部使用的
/// `kIOUSBDeviceUserClientTypeID` 等常量是同一对象（本机实测指针相等）。
unsafe fn constant_uuid(bytes: [u8; 16]) -> CFUUIDRef {
    // SAFETY: 16 个字节按序展开；返回的是常量 UUID，不需要释放。
    unsafe {
        CFUUIDGetConstantUUIDWithBytes(
            ptr::null(),
            bytes[0],
            bytes[1],
            bytes[2],
            bytes[3],
            bytes[4],
            bytes[5],
            bytes[6],
            bytes[7],
            bytes[8],
            bytes[9],
            bytes[10],
            bytes[11],
            bytes[12],
            bytes[13],
            bytes[14],
            bytes[15],
        )
    }
}

fn platform(code: KernReturn) -> TransportError {
    TransportError::Platform {
        code: i64::from(code),
    }
}

/// IOKit 插件对象的 RAII 包装。
///
/// `0` 是**句柄**（IOKit 分配的对象指针）：`*self.0` 才是接口结构（方法指针表），
/// 而方法的 `thisPointer` 必须是句柄本身。
struct PlugInInterface(*mut *mut IocfPlugInInterface);

impl PlugInInterface {
    /// 接口结构（方法表）；`thisPointer` 用 [`PlugInInterface::0`]。
    fn interface_vtable(&self) -> *mut IocfPlugInInterface {
        // SAFETY: 句柄由 IOCreatePlugInInterfaceForService 写入且非空，本结构持有期间有效。
        unsafe { *self.0 }
    }

    /// `QueryInterface(kIOUSBDeviceInterfaceID100)` 取设备接口（只读方法可用，不需要 open）。
    unsafe fn query_device_interface(&self) -> Result<DeviceInterface, TransportError> {
        // SAFETY: 句柄与接口结构都有效；QueryInterface 成功时返回已 AddRef 的设备句柄。
        unsafe {
            let query_interface = (*self.interface_vtable())
                .guts
                .query_interface
                .ok_or(platform(kIOReturnError))?;
            let mut device: *mut *mut IOUsbDeviceInterface = ptr::null_mut();
            let hr = query_interface(
                self.0 as *mut c_void,
                CFUUIDGetUUIDBytes(constant_uuid(IOUSB_DEVICE_INTERFACE_ID100)),
                ptr::from_mut(&mut device).cast::<*mut c_void>(),
            );
            if hr < 0 || device.is_null() {
                return Err(platform(hr));
            }
            Ok(DeviceInterface(device))
        }
    }
}

impl Drop for PlugInInterface {
    fn drop(&mut self) {
        // SAFETY: 句柄由 IOCreatePlugInInterfaceForService 分配，只在此处 Release 一次。
        unsafe {
            let release = (*self.interface_vtable()).guts.release;
            if let Some(release) = release {
                release(self.0 as *mut c_void);
            }
        }
    }
}

/// `IOUSBDeviceInterface` 对象的 RAII 包装（句柄语义同 [`PlugInInterface`]）。
struct DeviceInterface(*mut *mut IOUsbDeviceInterface);

impl DeviceInterface {
    /// `GetConfigurationDescriptorPtr(config_index)` 取配置描述符字节（只读，不打开设备）。
    unsafe fn configuration_descriptor(&self, config_index: u8) -> Result<Vec<u8>, TransportError> {
        // SAFETY: 句柄与接口结构有效；返回指针指向设备自身描述符数据，只在对象存活期内读取。
        unsafe {
            let vtable = *self.0;
            let get_descriptor = (*vtable)
                .get_configuration_descriptor_ptr
                .ok_or(platform(kIOReturnError))?;
            let mut descriptor: *const c_void = ptr::null();
            let kr = get_descriptor(self.0 as *mut c_void, config_index, &mut descriptor);
            if kr != KERN_SUCCESS {
                return Err(platform(kr));
            }
            if descriptor.is_null() {
                return Err(platform(kIOReturnBadArgument));
            }

            // `wTotalLength` 位于描述符偏移 +2（LE）；只按该长度拷贝，不做任何猜测性读取。
            let head = std::slice::from_raw_parts(descriptor.cast::<u8>(), 4);
            let total_length = usize::from(u16::from_le_bytes([head[2], head[3]]));
            if total_length < 4 {
                return Err(platform(kIOReturnBadArgument));
            }
            Ok(std::slice::from_raw_parts(descriptor.cast::<u8>(), total_length).to_vec())
        }
    }
}

impl Drop for DeviceInterface {
    fn drop(&mut self) {
        // SAFETY: 句柄由 QueryInterface 返回（已 AddRef），只在此处 Release 一次。
        unsafe {
            let release = (*(*self.0)).guts.release;
            if let Some(release) = release {
                release(self.0 as *mut c_void);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 槽位核对：`GetConfigurationDescriptorPtr` 必须是第 21 个指针槽（SDK `offsetof` = 168）。
    ///
    /// 槽位数算错会调用到别的设备方法（例如打开设备/接口的占用型方法），是必须被拦住的错误。
    #[test]
    fn test_iokit_vtable_layout_matches_iousblib() {
        let slot = std::mem::size_of::<*const c_void>();
        assert_eq!(
            std::mem::offset_of!(IOUsbDeviceInterface, get_configuration_descriptor_ptr),
            GET_CONFIGURATION_DESCRIPTOR_PTR_SLOT * slot
        );
        assert_eq!(
            std::mem::offset_of!(IocfPlugInInterface, _version),
            4 * slot,
            "IOCFPlugInInterface 的 version 紧随 IUNKNOWN_C_GUTS 四个槽位"
        );
        assert_eq!(
            std::mem::offset_of!(IocfPlugInInterface, _version),
            std::mem::offset_of!(IUnknownGuts, _reserved) + 4 * slot
        );
    }

    /// 常量 UUID 的字节序自检：写进 FFI 的 16 字节必须原样读回。
    #[test]
    fn test_constant_uuid_round_trip() {
        // SAFETY: 两个函数都只处理 CoreFoundation 常量 UUID 与定长字节结构。
        unsafe {
            for bytes in [
                IOUSB_DEVICE_USER_CLIENT_TYPE_ID,
                IOCF_PLUGIN_INTERFACE_ID,
                IOUSB_DEVICE_INTERFACE_ID100,
            ] {
                let read_back = CFUUIDGetUUIDBytes(constant_uuid(bytes));
                let actual = [
                    read_back.byte0,
                    read_back.byte1,
                    read_back.byte2,
                    read_back.byte3,
                    read_back.byte4,
                    read_back.byte5,
                    read_back.byte6,
                    read_back.byte7,
                    read_back.byte8,
                    read_back.byte9,
                    read_back.byte10,
                    read_back.byte11,
                    read_back.byte12,
                    read_back.byte13,
                    read_back.byte14,
                    read_back.byte15,
                ];
                assert_eq!(actual, bytes);
            }
        }
    }
}
