# entitlement 实测矩阵（macOS 26.6 arm64，SIP enabled，无任何 codesigning 证书）

| # | dext entitlement | 宿主 entitlement | 签名 | 结果 | 证据 |
|---|---|---|---|---|---|
| M1 | （空） | com.apple.developer.system-extension.install | 全 ad-hoc | 宿主进程 exec 阶段被 AMFI SIGKILL（exit 137） | amfid: "Adhoc signed app with restricted entitlements detected"；AMFI Code -424 "The file is adhoc signed but contains restricted entitlements"；kernel: "bailing out because of restricted entitlements" → amfi-kill-m1.log |
| M2 | （空） | （空） | 全 ad-hoc | 请求送达 sysextd，返回 OSSystemExtensionErrorDomain code=2（missingEntitlement） | m2-missing-entitlement.log |
| M3 | allow-any-userclient-access | 任意 | ad-hoc | 不可达：激活请求在宿主 entitlement/dext 校验之前已被 M1/M2 拒绝 | 同上 |
| — | developer mode 路线 | — | — | `systemextensionsctl developer on` 在 SIP enabled 下直接拒绝（工具原文见 developer-on-sip.log） | developer-on-sip.log |

## 死锁闭环
com.apple.developer.system-extension.install 是受限 entitlement：
- 宿主带它 + ad-hoc → AMFI exec 阶段 SIGKILL（M1）
- 宿主不带它 → sysextd 返回 missingEntitlement（M2）
- 解锁条件：真实 Apple Development/Developer ID 证书 + 覆盖该 entitlement 的 provisioning profile
  （免费账号 Apple Development 证书即可满足开发装载；正式分发另需 Developer ID + 审批）
