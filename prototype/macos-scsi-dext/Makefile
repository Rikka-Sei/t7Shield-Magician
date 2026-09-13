# T7 Shield SCSIPeripheralsDriverKit 只读原型 —— 构建脚本
#
# 目标：
#   make          —— 编译 dext 二进制 + 宿主二进制 + 组装 .app + ad-hoc 签名
#   make sign-any-uc —— 以 allow-any-userclient-access entitlement 重签 dext（矩阵 M2）
#   make sign-none    —— 以空 entitlement 重签 dext（矩阵 M1）
#   make activate —— 提交 system extension 激活请求
#   make send     —— 连接 user client 并发 Level-0 Discovery CDB
#   make uninstall—— 卸载 dext
#
# 环境变量：DDK_SDK（默认自动定位 Xcode 的 DriverKit.sdk）

DDK_SDK ?= $(shell ls -d /Applications/Xcode.app/Contents/Developer/Platforms/DriverKit.platform/Developer/SDKs/DriverKit.sdk 2>/dev/null)
XCODE    := /Applications/Xcode.app/Contents/Developer
IIG      := $(XCODE)/Toolchains/XcodeDefault.xctoolchain/usr/bin/iig
CLANG    := $(XCODE)/Toolchains/XcodeDefault.xctoolchain/usr/bin/clang++

DK_FRAMEWORKS := $(DDK_SDK)/System/DriverKit/System/Library/Frameworks

APP     := build/T7ProtoHost.app
DEXT    := $(APP)/Contents/Library/SystemExtensions/t7-discovery.dext
ENT_DIR := entitlements

.PHONY: all sign-none sign-any-uc activate send uninstall clean

all: $(APP) sign-none

# ---------- dext ----------
build/iig/T7DiscoveryDriver.h build/iig/T7DiscoveryDriver.cpp: dext/T7DiscoveryDriver.iig
	@mkdir -p build/iig
	$(IIG) --def $< --header build/iig/T7DiscoveryDriver.h --impl build/iig/T7DiscoveryDriver.cpp --deployment-target 12.0 -- -D__IIG=1 -x c++ -std=c++17 -isysroot $(DDK_SDK) -F $(DK_FRAMEWORKS) -I $(DDK_SDK)/System/DriverKit/System/Library/Frameworks

build/iig/T7DiscoveryUserClient.h build/iig/T7DiscoveryUserClient.cpp: dext/T7DiscoveryUserClient.iig
	@mkdir -p build/iig
	$(IIG) --def $< --header build/iig/T7DiscoveryUserClient.h --impl build/iig/T7DiscoveryUserClient.cpp --deployment-target 12.0 -- -D__IIG=1 -x c++ -std=c++17 -isysroot $(DDK_SDK) -F $(DK_FRAMEWORKS) -I $(DDK_SDK)/System/DriverKit/System/Library/Frameworks

build/t7-discovery: dext/T7DriverImpl.cpp build/iig/T7DiscoveryDriver.h build/iig/T7DiscoveryDriver.cpp build/iig/T7DiscoveryUserClient.h build/iig/T7DiscoveryUserClient.cpp
	$(CLANG) -x c++ -std=c++17 -fno-rtti -fno-exceptions -fno-modules \
	  -target arm64-apple-driverkit23.0 \
	  -isysroot $(DDK_SDK) \
	  -F $(DK_FRAMEWORKS) \
	  -framework DriverKit -framework SCSIPeripheralsDriverKit \
	  -Ibuild/iig \
	  -Wall \
	  $^ -bundle -o $@

$(DEXT)/Contents/MacOS/t7-discovery: build/t7-discovery dext/Info.plist
	@mkdir -p $(DEXT)/Contents/MacOS
	cp dext/Info.plist $(DEXT)/Contents/Info.plist
	cp build/t7-discovery $(DEXT)/Contents/MacOS/t7-discovery
	@touch $(DEXT)

# ---------- host ----------
build/activate: host/activate.m
	$(CLANG) -x objective-c -fobjc-arc -isysroot $(XCODE)/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk \
	  -framework Foundation -framework SystemExtensions $< -o $@

build/send: host/send.c
	$(CLANG) -isysroot $(XCODE)/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk \
	  -framework IOKit -framework CoreFoundation $< -o $@


$(APP)/Contents/MacOS/activate: build/activate host/App-Info.plist
	@mkdir -p $(APP)/Contents/MacOS $(APP)/Contents/Library/SystemExtensions
	cp host/App-Info.plist $(APP)/Contents/Info.plist
	cp build/activate $(APP)/Contents/MacOS/activate

$(APP): $(DEXT)/Contents/MacOS/t7-discovery $(APP)/Contents/MacOS/activate

# ---------- 签名（entitlement 矩阵） ----------
sign-none: $(APP)
	codesign --force --sign - --entitlements $(ENT_DIR)/dext-none.entitlements $(DEXT)
	codesign --force --sign - --entitlements $(ENT_DIR)/host-install.entitlements $(APP)
	@echo "signed: dext without family entitlements (M1)"

sign-any-uc: $(APP)
	codesign --force --sign - --entitlements $(ENT_DIR)/dext-any-userclient.entitlements $(DEXT)
	codesign --force --sign - --entitlements $(ENT_DIR)/host-install.entitlements $(APP)
	@echo "signed: dext with allow-any-userclient-access (M2)"

# ---------- 运行 ----------
activate:
	$(APP)/Contents/MacOS/activate $(APP)

send:
	build/send $(filter-out,$(MAKECMDGOALS),)

uninstall:
	-systemextensionsctl uninstall - com.t7shield.proto.t7-discovery
	@echo "uninstall submitted"

clean:
	rm -rf build
