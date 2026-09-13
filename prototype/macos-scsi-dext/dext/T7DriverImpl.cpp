/*
 * T7 Shield Level-0 Discovery 只读原型 dext —— 实现。
 * 只允许发送一条只读 CDB（SECURITY PROTOCOL IN / TCG Level-0 Discovery）。
 */
#include <os/log.h>
#include <string.h>

#include <DriverKit/DriverKit.h>
#include <DriverKit/IOUserServer.h>
#include <DriverKit/IOBufferMemoryDescriptor.h>
#include <DriverKit/IOMemoryDescriptor.h>
#include <DriverKit/IOMemoryMap.h>
#include <DriverKit/OSData.h>
#include <SCSIPeripheralsDriverKit/IOUserSCSIPeripheralDeviceType00.h>
#include <SCSIPeripheralsDriverKit/IOUserSCSIPeripheralDeviceHelper.h>

#include "T7DiscoveryDriver.h"
#include "T7DiscoveryUserClient.h"

#define LOG_PREFIX "t7-dext"
#define dlog(...) os_log(OS_LOG_DEFAULT, LOG_PREFIX ": " __VA_ARGS__)

/* 安全红线：全 dext 唯一允许的 CDB（SECURITY PROTOCOL IN, TCG, Level 0 Discovery） */
static const uint8_t kDiscoveryCDB[16] = {
    0xA2, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
    0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00
};
#define DISCOVERY_BUF_SIZE 512u   /* 响应缓冲；Level-0 发现响应通常 0x44~0x200 字节 */
#define SENSE_BUF_SIZE     64u
#define SENSE_LEN_REQ      18u
#define TIMEOUT_MS         30000u /* 与盘上 Protocol Characteristics 的默认超时一致 */

bool
T7DiscoveryDriver::init()
{
    dlog("driver init");
    return super::init();
}

void
T7DiscoveryDriver::free()
{
    dlog("driver free");
    super::free();
}

kern_return_t
IMPL(T7DiscoveryDriver, Start)
{
    dlog("Start provider=%{public}s", provider ? "IOSCSIHierarchicalLogicalUnit" : "(null)");
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) {
        dlog("super::Start failed 0x%08x", ret);
        return ret;
    }
    RegisterService();
    dlog("Start done, registered");
    return kIOReturnSuccess;
}

kern_return_t
IMPL(T7DiscoveryDriver, Stop)
{
    dlog("Stop");
    return Stop(provider, SUPERDISPATCH);
}

kern_return_t
IMPL(T7DiscoveryDriver, NewUserClient)
{
    dlog("NewUserClient type=%u", type);
    IOService * client = nullptr;
    kern_return_t kr = Create(this, "UserClientProperties", &client);
    if (kr != kIOReturnSuccess) {
        dlog("Create UserClientProperties failed 0x%08x", kr);
        return kr;
    }
    *userClient = OSDynamicCast(IOUserClient, client);
    if (*userClient == nullptr) {
        client->release();
        dlog("created object is not IOUserClient");
        return kIOReturnError;
    }
    return kIOReturnSuccess;
}

kern_return_t
IMPL(T7DiscoveryDriver, UserDetermineDeviceCharacteristics)
{
    dlog("UserDetermineDeviceCharacteristics");
    *result = true;
    return kIOReturnSuccess;
}

/* ---------------- user client ---------------- */

bool
T7DiscoveryUserClient::init()
{
    if (!super::init()) {
        return false;
    }
    dlog("user client init");
    return true;
}

void
T7DiscoveryUserClient::free()
{
    dlog("user client free");
    super::free();
}

kern_return_t
T7DiscoveryUserClient::SendDiscovery(IOUserClientMethodArguments * arguments)
{
    T7DiscoveryDriver * driver = OSDynamicCast(T7DiscoveryDriver, GetProvider());
    if (driver == nullptr) {
        dlog("provider is not T7DiscoveryDriver");
        return kIOReturnNotAttached;
    }

    IOBufferMemoryDescriptor * dataBuf  = nullptr;
    IOBufferMemoryDescriptor * senseBuf = nullptr;
    IOMemoryMap              * outMap   = nullptr;
    kern_return_t ret;

    ret = IOBufferMemoryDescriptor::Create(kIOMemoryDirectionInOut,
                                           DISCOVERY_BUF_SIZE, 0, &dataBuf);
    if (ret != kIOReturnSuccess) {
        dlog("data buffer create failed 0x%08x", ret);
        return ret;
    }
    ret = IOBufferMemoryDescriptor::Create(kIOMemoryDirectionInOut,
                                           SENSE_BUF_SIZE, 0, &senseBuf);
    if (ret != kIOReturnSuccess) {
        dlog("sense buffer create failed 0x%08x", ret);
        dataBuf->release();
        return ret;
    }

    IOAddressSegment dataSeg  = {};
    IOAddressSegment senseSeg = {};
    dataBuf->GetAddressRange(&dataSeg);
    senseBuf->GetAddressRange(&senseSeg);
    memset((void *)(uintptr_t)dataSeg.address, 0, DISCOVERY_BUF_SIZE);
    memset((void *)(uintptr_t)senseSeg.address, 0, SENSE_BUF_SIZE);

    SCSIType00OutParameters request;
    SCSIType00InParameters  response;
    memset(&request, 0, sizeof request);
    memset(&response, 0, sizeof response);

    request.fLogicalUnitNumber = 0;
    request.fTimeoutDuration   = TIMEOUT_MS;
    memcpy(request.fCommandDescriptorBlock, kDiscoveryCDB, sizeof kDiscoveryCDB);
    request.fRequestedByteCountOfTransfer = DISCOVERY_BUF_SIZE;
    request.fBufferDirection        = kSCSIDataTransfer_FromTargetToInitiator;
    request.fDataTransferDirection  = kSCSIDataTransfer_FromTargetToInitiator;
    request.fSenseLengthRequested   = SENSE_LEN_REQ;
    request.fDataBufferAddr         = dataSeg.address;
    request.fSenseBufferAddr        = senseSeg.address;

    ret = driver->UserSendCDB(request, &response);
    dlog("UserSendCDB ret=0x%08x status=%u svcResp=0x%x realized=%llu senseValid=%d",
         ret, (unsigned)response.fCompletionStatus,
         (unsigned)response.fServiceResponse,
         (unsigned long long)response.fRealizedByteCountOfTransfer,
         (int)response.fSenseDataValid);

    uint64_t realized = response.fRealizedByteCountOfTransfer;
    if (realized > DISCOVERY_BUF_SIZE) {
        realized = DISCOVERY_BUF_SIZE;
    }

    if (ret == kIOReturnSuccess) {
        if (arguments->structureOutputDescriptor != nullptr) {
            /* 宿主走 IOConnectCallStructMethod：写入调用者提供的描述符 */
            ret = arguments->structureOutputDescriptor->CreateMapping(
                0, 0, 0, 0, 0, &outMap);
            if (ret == kIOReturnSuccess && outMap != nullptr) {
                uint64_t outLen = realized;
                if (outLen > arguments->structureOutputMaximumSize) {
                    outLen = arguments->structureOutputMaximumSize;
                }
                memcpy((void *)(uintptr_t)outMap->GetAddress(),
                       (const void *)(uintptr_t)dataSeg.address,
                       (size_t)outLen);
            } else {
                dlog("CreateMapping failed 0x%08x", ret);
            }
        } else if (arguments->structureOutputMaximumSize >= realized
                   || arguments->structureOutputMaximumSize
                      == kIOUserClientVariableStructureSize) {
            /* 宿主未提供缓冲时回 OSData */
            OSData * out = OSData::withBytes(
                (const void *)(uintptr_t)dataSeg.address, (unsigned)realized);
            if (out != nullptr) {
                arguments->structureOutput = out; /* 由框架释放 */
            } else {
                ret = kIOReturnNoMemory;
            }
        } else {
            dlog("output too small max=%llu realized=%llu",
                 (unsigned long long)arguments->structureOutputMaximumSize,
                 (unsigned long long)realized);
            ret = kIOReturnNoSpace;
        }
    }

    if (outMap != nullptr)  outMap->release();
    if (senseBuf != nullptr) senseBuf->release();
    if (dataBuf != nullptr)  dataBuf->release();
    return ret;
}
