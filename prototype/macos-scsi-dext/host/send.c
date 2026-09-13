/*
 * T7 Shield dext 原型 —— 宿主 CLI：连接 custom user client，发只读
 * Level-0 Discovery CDB，打印响应 hex 并解析 Opal SSC 描述符。
 *
 * 用法：send [hex|full]
 *   hex  （默认）打印响应前 64 字节 hex
 *   full 打印整个响应
 */
#include <IOKit/IOKitLib.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define SELECTOR_SEND_DISCOVERY 0
#define BUF_SIZE 512

static void hexdump(const unsigned char * buf, size_t len)
{
    for (size_t i = 0; i < len; i += 16) {
        printf("%04zx  ", i);
        for (size_t j = 0; j < 16; ++j) {
            if (i + j < len) printf("%02x ", buf[i + j]);
            else             printf("   ");
            if (j == 7) putchar(' ');
        }
        printf(" |");
        for (size_t j = 0; j < 16 && i + j < len; ++j) {
            unsigned char c = buf[i + j];
            putchar(c >= 0x20 && c < 0x7f ? c : '.');
        }
        printf("|\n");
    }
}

int main(int argc, char ** argv)
{
    int full = argc > 1 && strcmp(argv[1], "full") == 0;

    io_iterator_t iter = 0;
    kern_return_t kr = IOServiceGetMatchingServices(
        kIOMainPortDefault,
        IOServiceMatching("T7DiscoveryDriver"),
        &iter);
    if (kr != KERN_SUCCESS) {
        fprintf(stderr, "IOServiceGetMatchingServices: 0x%08x\n", kr);
        return 1;
    }
    io_service_t service = IOIteratorNext(iter);
    IOObjectRelease(iter);
    if (!service) {
        fprintf(stderr, "dext driver instance not found in IORegistry（未匹配/未激活？）\n");
        return 2;
    }

    io_connect_t connect = 0;
    kr = IOServiceOpen(service, mach_task_self(), 0, &connect);
    IOObjectRelease(service);
    if (kr != KERN_SUCCESS) {
        fprintf(stderr, "IOServiceOpen: 0x%08x (kIOReturn*)\n", kr);
        return 3;
    }

    unsigned char out[BUF_SIZE];
    memset(out, 0, sizeof out);
    size_t outLen = sizeof out;
    kr = IOConnectCallStructMethod(connect, SELECTOR_SEND_DISCOVERY,
                                   NULL, 0, out, &outLen);
    if (kr != KERN_SUCCESS) {
        fprintf(stderr, "IOConnectCallStructMethod(selector 0): 0x%08x\n", kr);
        IOServiceClose(connect);
        return 4;
    }

    printf("realized bytes: %zu\n", outLen);
    size_t dumpLen = full ? outLen : (outLen > 64 ? 64 : outLen);
    hexdump(out, dumpLen);

    int ret = 0;
    if (outLen >= 0x32) {
        unsigned desc0 = ((unsigned)out[0x30] << 8) | out[0x31];
        unsigned totalLen = ((unsigned)out[0x2A] << 24) | ((unsigned)out[0x2B] << 16)
                          | ((unsigned)out[0x2C] << 8) | out[0x2D];
        printf("Level-0 header: TotalLength=%u (0x%x), first descriptor type=0x%04x\n",
               totalLen, totalLen, desc0);
        if (desc0 == 0x0203) {
            printf("SUCCESS: Opal SSC (0x0203) descriptor found at 0x30\n");
        } else {
            printf("NOTE: first descriptor at 0x30 is NOT 0x0203 (Opal SSC)\n");
            ret = 5;
        }
    } else {
        printf("NOTE: response shorter than 0x32 bytes, no descriptor parse\n");
        ret = 6;
    }

    IOServiceClose(connect);
    return ret;
}
