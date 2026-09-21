# D33 setfacl 切换 + 审计代码修订 + magi-app 全量文件重组 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 落地 spec 0.4（D33：`pkexec setfacl` 即时 ACL 权限修复），完成双审计代码修订（D1–D7），并按用户裁定执行 magi-app 全量文件重组（controller 三拆 + UI 按页拆分），最终全绿、真机验证、统一切回 Authoritative 并签名提交。

**Architecture:** 权限修复从 udev 规则脚本（NixOS 只读 store 死路）切换为 argv 直传 `pkexec setfacl` 即时 ACL；一次性自动修复纪律收进纯状态机（`AutoAttempts`）。文件组织执行两个约束：纯逻辑层每文件单一关注点（gate/rescan/jobs/environment），UI 层按窗口/对话框/页面分文件（D29 组织约束）。事件路由改窗口自持 Receiver，启动与环境自检全异步。

**Tech Stack:** Rust 2021 / gtk4 0.11 + libadwaita 0.9 / async-channel / rust-i18n（zh-CN + en）/ cargo test / spec 审计三件套（python3）。

**Spec:** `docs/specs/t7-magician/spec.md`（0.4，D01–D33，Draft——本计划完成后切 Authoritative）。

## Global Constraints

- 依赖单向：`magi-app` → `magi-protocol` → `magi-transport`；UI 不构造 CDB/令牌流（§3.1）。
- 用户可见文案一律 i18n 键，代码零内联（AC-014/015）；zh-CN 与 en 键集合一致（有测试断言）。
- workspace lints：`clippy correctness = deny`。
- 提权边界（§6/D33）：仅 `pkexec modprobe sg` 与 `pkexec setfacl`（argv 直传、`user`/`nodes` 白名单、不经 shell）；前置不满足不发起 pkexec，按修复失败呈现手动指引；零持久化。
- 自动修复纪律（§4.13）：启动引导链内由 `AutoAttempts` 各至多一次；`Ready` 后复发与失败重试归用户；UI 不得另设补偿标志。
- 周期重扫每轮附带环境自检；环境自检与扫描不阻塞 UI 首帧（§6）。
- 组织约束（D29）：界面代码按窗口/对话框/页面分文件，单文件单一界面单元；纯逻辑模块每文件单一关注点。
- 提交：中文 conventional commits + GPG 签名（`git commit -S`，密钥 `85E52EEE42578D11`）。
- 锚点改名（用户已批准）：`test_window_size_scales_with_workarea` → `test_window_size_scales_with_display`，代码 + manifest + spec §10 三处同步。
- 现状：environment.rs 有编译断点与两个同名草稿测试；controller.rs 834 行；ui/main_window.rs 1749 行。
- `mod test_support` 与各模块内 `#[cfg(test)]` 测试随代码一起搬迁（锚点 grep 范围是 `crates/magi-app/**/*.rs`，搬文件不动锚点可达性）。

- 目录分组（用户裁定）：gate/rescan/jobs/environment/observation 五个设备域纯逻辑模块收进 `src/device/` 子目录（`crate::device::…` 路径，净切换不留 re-export shim）；presentation/settings/diagnostics/test_support 横切模块与 ui/ 留在原位。manifest 契约/锚点 glob `crates/magi-app/src/**/*.rs` 覆盖子目录，零 manifest/spec 改动。

---

### Task 1: environment.rs 定稿（状态机 + 命名修订 + 锚点改名）

**Files:**
- Move: `crates/magi-app/src/environment.rs` → `crates/magi-app/src/device/environment.rs`（Step 0，git mv）
- Move: `crates/magi-app/src/observation.rs` → `crates/magi-app/src/device/observation.rs`（Step 0，git mv）
- Modify: `crates/magi-app/src/device/environment.rs`（Step 1–3）
- Modify: `crates/magi-app/src/lib.rs` + 新建 `crates/magi-app/src/device/mod.rs`（Step 0 模块声明）
- Modify: 全仓 `crate::environment`/`crate::observation` 引用点（Step 0）
- Modify: `crates/magi-app/src/ui/main_window.rs`（仅测试名引用处同步改名）
- Modify: `docs/specs/t7-magician/tools/audit_manifest.json`（锚点名）
- Modify: `docs/specs/t7-magician/spec.md`（§10 锚点行）

**Interfaces:**
- Produces: `crate::device::environment::{AutoAttempts, next_environment_state, permission_fix_commands, permission_fix_commands_with_path, current_user, username_is_allowed, sg_node_is_allowed}`（校验器与裁决器为新名：原 `user_is_safe`/`node_is_safe`/`startup_done`/`recheck_done` 改名，后两者私有）。

- [ ] **Step 0: 目录分组搬迁（纯 git mv + 路径更新，零行为变更）**

```bash
mkdir -p crates/magi-app/src/device
git mv crates/magi-app/src/environment.rs crates/magi-app/src/device/environment.rs
git mv crates/magi-app/src/observation.rs crates/magi-app/src/device/observation.rs
```

`lib.rs` 增 `pub mod device;`（`device/mod.rs` 声明 `pub mod environment; pub mod observation;`），删原两条根声明；全仓 `use crate::environment`/`crate::observation` 引用点改为 `crate::device::…`（grep 逐点更新，测试随迁）。跑 `cargo test -p magi-app 2>&1 | grep 'test result'` 确认数量不变。
- [ ] **Step 1: 重写测试区为失败态**

删除文件内两个 `test_permission_fix_command_whitelist` 草稿与旧二参调用，写入（完整代码与上一版计划 Task 1 Step 1 相同，仅两处差异——校验器改名、状态机助手改名后测试内引用同步）：

```rust
    /// 锚点（§10）：环境就绪状态机全表转换（含 AutoAttempts 一次性纪律，D33）。
    #[test]
    fn test_environment_state_transitions() {
        use EnvironmentAction as A;
        use EnvironmentEvent as E;
        use EnvironmentState as S;

        let missing = EnvironmentReport { issues: vec![EnvironmentIssue::SgModuleMissing] };
        let denied = EnvironmentReport {
            issues: vec![EnvironmentIssue::SgNodePermissionDenied { node: "/dev/sg0".to_string() }],
        };
        let ready = EnvironmentReport::default();
        let mut autos = AutoAttempts::default();

        // 启动：空 → Ready；模块缺失 → 自动装载（置位 autos.module）。
        let (s, a) = next_environment_state(S::Unknown, E::CheckDone(ready.clone()), &mut autos);
        assert_eq!((s, a), (S::Ready, A::HidePill));
        let (s, a) = next_environment_state(S::Unknown, E::CheckDone(missing.clone()), &mut autos);
        assert_eq!((s, a), (S::Loading(missing.issues.clone()), A::LoadModule));
        assert!(autos.module && !autos.permissions);

        // 装载成功复检模块仍在 → 不再自动装载（防循环），转 Issue。
        let (s, _) = next_environment_state(s, E::LoadSucceeded, &mut autos);
        let (s, a) = next_environment_state(s, E::CheckDone(missing.clone()), &mut autos);
        assert_eq!((s, a), (S::Issue(missing.issues.clone()), A::ShowPill));

        // 引导链串联：装载成功复检发现权限问题 → 自动修复（第二个标志位）。
        let mut autos = AutoAttempts::default();
        let (s, _) = next_environment_state(S::Unknown, E::CheckDone(missing.clone()), &mut autos);
        let (s, _) = next_environment_state(s, E::LoadSucceeded, &mut autos);
        let (s, a) = next_environment_state(s, E::CheckDone(denied.clone()), &mut autos);
        assert_eq!((s, a), (S::Fixing(denied.issues.clone()), A::FixPermissions));
        assert!(autos.module && autos.permissions);
        let (s, _) = next_environment_state(s, E::FixPermissionsSucceeded, &mut autos);
        let (s, a) = next_environment_state(s, E::CheckDone(ready.clone()), &mut autos);
        assert_eq!((s, a), (S::Ready, A::HidePill));

        // Ready 后复发 → Issue（不自动）；用户显式重试不受限。
        let (s, a) = next_environment_state(S::Ready, E::CheckDone(denied.clone()), &mut autos);
        assert_eq!((s, a), (S::Issue(denied.issues.clone()), A::ShowPill));
        let (s, a) = next_environment_state(s, E::LoadRequested, &mut autos);
        assert_eq!((s, a), (S::Loading(denied.issues.clone()), A::LoadModule));
        let (s, a) = next_environment_state(
            S::Issue(denied.issues.clone()),
            E::FixPermissionsRequested,
            &mut autos,
        );
        assert_eq!((s, a), (S::Fixing(denied.issues.clone()), A::FixPermissions));

        // 失败分支与单飞/总则。
        let (s, a) = next_environment_state(S::Loading(missing.issues.clone()), E::LoadFailed, &mut autos);
        assert_eq!((s, a), (S::LoadFailed(missing.issues.clone()), A::ShowPill));
        let (s, a) = next_environment_state(
            S::Fixing(denied.issues.clone()),
            E::FixPermissionsFailed,
            &mut autos,
        );
        assert_eq!((s, a), (S::Issue(denied.issues.clone()), A::ShowPill));
        for state in [S::Loading(vec![]), S::Fixing(vec![])] {
            for event in [E::LoadRequested, E::FixPermissionsRequested] {
                let got = next_environment_state(state.clone(), event, &mut autos);
                assert_eq!(got, (state, A::None));
            }
        }
        let got = next_environment_state(S::Ready, E::LoadRequested, &mut autos);
        assert_eq!(got, (S::Ready, A::None));
        assert_eq!(module_load_command(), &["pkexec", "modprobe", "sg"]);
    }

    /// 锚点（§10）：权限修复命令固定形态（D33）——argv 直传、user/nodes 白名单。
    #[test]
    fn test_permission_fix_command_whitelist() {
        let dir = tempdir();
        fs::write(dir.path().join("setfacl"), b"#!/bin/sh\n").unwrap();
        let path = format!("{}:/usr/bin", dir.path().display());
        let nodes = vec!["/dev/sg0".to_string(), "/dev/sg1".to_string()];

        let argv = permission_fix_commands_with_path(Some("rikki"), &nodes, Some(&path))
            .expect("合法输入必须构造出命令");
        assert_eq!(argv[0], "pkexec");
        assert!(argv[1].ends_with("/setfacl"));
        assert_eq!(argv[2], "-m");
        assert_eq!(argv[3], "u:rikki:rw");
        assert_eq!(argv[4..], nodes);

        assert!(permission_fix_commands_with_path(Some("rikki;rm"), &nodes, Some(&path)).is_none());
        assert!(permission_fix_commands_with_path(Some("a/b"), &nodes, Some(&path)).is_none());
        assert!(permission_fix_commands_with_path(None, &nodes, Some(&path)).is_none());
        assert!(permission_fix_commands_with_path(Some("rikki"), &[], Some(&path)).is_none());
        assert!(permission_fix_commands_with_path(
            Some("rikki"),
            &vec!["/dev/nvme0".to_string()],
            Some(&path)
        )
        .is_none());
        assert!(permission_fix_commands_with_path(Some("rikki"), &nodes, None).is_none());
        assert!(permission_fix_commands_with_path(Some("rikki"), &nodes, Some("/nonexistent")).is_none());

        assert!(username_is_allowed("a.b-c_d") && sg_node_is_allowed("/dev/sg12"));
        assert!(!username_is_allowed("") && !username_is_allowed("rikki;rm -rf /"));
        assert!(!sg_node_is_allowed("/dev/sg"));
        assert!(!sg_node_is_allowed("/dev/sg0/../../etc"));
        assert!(!sg_node_is_allowed("/dev/nvme0"));
    }
```

main_window.rs 测试区：`test_window_size_scales_with_workarea` 改名 `test_window_size_scales_with_display`（函数体不动）；manifest `test_anchors` 中同名条目与 spec §10 对应行同步改名（§10 行描述保持「随显示器几何推导、含上下限与无头回落（D31）」）。

- [ ] **Step 2: 确认失败**

Run: `cargo test -p magi-app --lib environment:: 2>&1 | tail -3`
Expected: 编译错误（三参未实现 / `AutoAttempts` 未定义 / 校验器新名未定义）。

- [ ] **Step 3: 实现状态机**

`EnvironmentState` 六变体不变；新增 `AutoAttempts`（`#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]`，`pub module: bool`、`pub permissions: bool`）；迁移函数与裁决器（`user_is_safe`→`username_is_allowed`、`node_is_safe`→`sg_node_is_allowed`、`startup_done`→`startup_verdict`、`recheck_done`→`recheck_verdict`）：

```rust
pub fn next_environment_state(
    state: EnvironmentState,
    event: EnvironmentEvent,
    autos: &mut AutoAttempts,
) -> (EnvironmentState, EnvironmentAction) {
    use EnvironmentAction as A;
    use EnvironmentEvent as E;
    use EnvironmentState as S;
    match (state, event) {
        (S::Unknown | S::Checking, E::CheckDone(report)) => startup_verdict(report, autos),
        (S::Loading(_), E::LoadSucceeded) => (S::Checking, A::None),
        (S::Loading(issues), E::LoadFailed) => (S::LoadFailed(issues), A::ShowPill),
        (S::Loading(issues), E::LoadRequested | E::FixPermissionsRequested) => {
            (S::Loading(issues), A::None)
        }
        (S::Fixing(_), E::FixPermissionsSucceeded) => (S::Checking, A::None),
        (S::Fixing(issues), E::FixPermissionsFailed) => (S::Issue(issues), A::ShowPill),
        (S::Fixing(issues), E::FixPermissionsRequested | E::LoadRequested) => {
            (S::Fixing(issues), A::None)
        }
        (S::Issue(issues) | S::LoadFailed(issues), E::LoadRequested) => {
            (S::Loading(issues), A::LoadModule)
        }
        (S::Issue(issues) | S::LoadFailed(issues), E::FixPermissionsRequested) => {
            (S::Fixing(issues), A::FixPermissions)
        }
        (S::Issue(_) | S::LoadFailed(_) | S::Ready, E::CheckDone(report)) => recheck_verdict(report),
        (state, _) => (state, A::None),
    }
}

/// 启动引导链裁决：自动装载/修复各至多一次（AutoAttempts 置位），循环由构造排除。
fn startup_verdict(
    report: EnvironmentReport,
    autos: &mut AutoAttempts,
) -> (EnvironmentState, EnvironmentAction) {
    use EnvironmentAction as A;
    use EnvironmentState as S;
    if report.issues.is_empty() {
        return (S::Ready, A::HidePill);
    }
    let module_missing = report
        .issues
        .iter()
        .any(|issue| matches!(issue, EnvironmentIssue::SgModuleMissing));
    let permission_denied = report
        .issues
        .iter()
        .any(|issue| matches!(issue, EnvironmentIssue::SgNodePermissionDenied { .. }));
    if module_missing && !autos.module {
        autos.module = true;
        (S::Loading(report.issues), A::LoadModule)
    } else if permission_denied && !module_missing && !autos.permissions {
        autos.permissions = true;
        (S::Fixing(report.issues), A::FixPermissions)
    } else {
        (S::Issue(report.issues), A::ShowPill)
    }
}
```

`recheck_verdict` 逻辑不变（空 → `Ready`+`HidePill`；非空 → `Issue`+`ShowPill`）；删除全部 udev 常量残留与草稿。

- [ ] **Step 4: 与 Task 3 合并后跑测试（见 Task 3 Step 4）**

---

### Task 2: controller.rs 拆解（gate.rs / rescan.rs / jobs 吸收，controller 删除）

**Files:**
- Create: `crates/magi-app/src/device/gate.rs`（ActionId、UnlockGate、PasswordAttempts 及其测试）
- Create: `crates/magi-app/src/device/rescan.rs`（DeviceIdentity、RescanState、clamp_devices 及其测试）
- Move + Modify: `crates/magi-app/src/jobs.rs` → `crates/magi-app/src/device/jobs.rs`（Step 0 git mv；吸收 DeviceId、JobRegistry、JobGuard、open_and_discover 及其测试）
- Modify: `crates/magi-app/src/presentation.rs`（吸收 PlatformNotice）
- Delete: `crates/magi-app/src/controller.rs`
- Modify: `crates/magi-app/src/lib.rs`（模块声明收敛进 `device/mod.rs`：`pub mod gate; pub mod rescan; pub mod jobs;`，删 `pub mod controller;` 与根 `pub mod jobs;`）
- Modify: 全部 `use crate::controller::…` 与 `crate::jobs::…` 引用点（main_window.rs / presentation.rs / ui/*.rs，以 grep 为准逐一改为 `crate::device::…` 路径）

**Interfaces:**
- Produces: `crate::device::gate::{ActionId, UnlockGate}`、`crate::device::rescan::{DeviceIdentity, RescanState, clamp_devices}`、`crate::device::jobs::{DeviceId, JobRegistry, open_and_discover}`——公开签名与实现原样搬迁（零行为变更），仅模块路径变化。

- [ ] **Step 1: 机械搬迁（编译器驱动）**

按 controller.rs 现有分节把符号与对应 `#[cfg(test)]` 测试整块移入目标文件；`lib.rs` 增删模块声明；`grep -rn 'crate::controller' crates/magi-app/src` 的每个引用点改路径。搬迁不改任何逻辑、不改任何函数签名。

- [ ] **Step 2: 全量测试（行为零变化验证）**

Run: `cargo test -p magi-app 2>&1 | grep -E 'test result|error' | head -5`
Expected: PASS，测试数量与搬迁前一致（锚点随代码迁移仍可 grep）。

- [ ] **Step 3: 提交**

```bash
git add -A crates/magi-app/src
git commit -S -m "refactor: controller.rs 拆解为 gate/rescan，作业设施并入 jobs（单一关注点分文件）

- gate.rs：ActionId/UnlockGate/口令重试预算（入口启用矩阵）
- rescan.rs：DeviceIdentity/RescanState/clamp_devices（重扫呈现裁决）
- jobs.rs：吸收 DeviceId/JobRegistry/open_and_discover（定义与唯一消费者同文件）
- presentation.rs：吸收 PlatformNotice；controller.rs 删除（零行为变更，测试随迁）"
```

---

### Task 3: ui/environment_flow.rs 新建 + main_window 环境接线

**Files:**
- Create: `crates/magi-app/src/ui/environment_flow.rs`
- Modify: `crates/magi-app/src/ui/main_window.rs`（WindowState 换 `env_autos`；环境方法整体迁出；`state()` 改 `pub(crate)`）
- Modify: `crates/magi-app/src/ui/mod.rs`（`pub mod environment_flow;`）

**Interfaces:**
- Consumes: Task 1 全部产物。
- Produces: `impl MainWindow` 块（位于 environment_flow.rs）：`apply_environment_event`、`spawn_module_load`、`spawn_permission_fix`、`environment_snapshot`、`present_environment_dialog`、`refresh_environment_dialog`、`pub fn environment_load_requested`、`pub fn environment_fix_requested`、`pub fn environment_recheck_requested`。

- [ ] **Step 1: main_window.rs 侧改造**

1. `WindowState`：删 `auto_load_used`/`auto_fix_used`，加 `pub(crate) env_autos: crate::device::environment::AutoAttempts`（WindowState 字段当前私有，环境方法跨文件访问需 `pub(crate)`——同批把 WindowState 字段全部改 `pub(crate)`，供后续页面拆分复用）。
2. `fn state()` → `pub(crate) fn state()`。
3. 删除 main_window.rs 内的环境方法九个（Task 3 Step 3 的新文件承接），`start()`/`connect_actions` 中的调用点保持不变（方法经 `impl MainWindow` 跨文件可见）。

- [ ] **Step 2: 新文件骨架（含完整核心函数）**

```rust
//! 环境编排（D30/D33）：环境状态机宿主——事件入口、装载/修复工作线程、
//! 胶囊显隐与引导向导接线。界面元素在 main_window 构建，本文件只做编排（D29 分文件）。

use gtk::glib;

use crate::device::environment::{self, EnvironmentEvent, EnvironmentState, next_environment_state};
use crate::ui::MainWindow;
use crate::ui::environment_dialog::EnvironmentDialog;

impl MainWindow {
    /// 环境状态机入口（§4.13）：纯迁移 + 动作落位（一次性纪律由状态机与 env_autos 保证）。
    pub(crate) fn apply_environment_event(&self, event: EnvironmentEvent) {
        let action = {
            let mut state = self.state().borrow_mut();
            let (_next, action) =
                next_environment_state(state.environment.clone(), event, &mut state.env_autos);
            state.environment = _next;
            action
        };
        match action {
            environment::EnvironmentAction::None => {}
            environment::EnvironmentAction::ShowPill => {
                self.imp().environment_pill.set_visible(true)
            }
            environment::EnvironmentAction::HidePill => {
                self.imp().environment_pill.set_visible(false)
            }
            environment::EnvironmentAction::LoadModule => self.spawn_module_load(),
            environment::EnvironmentAction::FixPermissions => self.spawn_permission_fix(),
        }
        self.refresh_environment_dialog();
    }

    /// 模块装载（§4.13）：固定命令在工作线程执行，结果回主线程复检。
    pub(crate) fn spawn_module_load(&self) { /* 与现实现一致迁入（fetch_scan 环境复检不变） */ }

    /// 权限修复（§4.13，D33）：argv 直传 pkexec setfacl；前置不满足不发起、直接手动指引。
    pub(crate) fn spawn_permission_fix(&self) {
        let nodes: Vec<String> = {
            let state = self.state().borrow();
            match &state.environment {
                EnvironmentState::Issue(issues)
                | EnvironmentState::LoadFailed(issues)
                | EnvironmentState::Fixing(issues) => issues
                    .iter()
                    .filter_map(|issue| match issue {
                        environment::EnvironmentIssue::SgNodePermissionDenied { node } => {
                            Some(node.clone())
                        }
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            }
        };
        let Some(argv) =
            environment::permission_fix_commands(environment::current_user().as_deref(), &nodes)
        else {
            self.apply_environment_event(EnvironmentEvent::FixPermissionsFailed);
            return;
        };
        let (sender, receiver) = async_channel::unbounded::<bool>();
        std::thread::spawn(move || {
            let ok = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            let _ = sender.send_blocking(ok);
        });
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            if let Ok(ok) = receiver.recv().await {
                this.apply_environment_event(if ok {
                    EnvironmentEvent::FixPermissionsSucceeded
                } else {
                    EnvironmentEvent::FixPermissionsFailed
                });
                let report = environment::inspect_environment();
                this.apply_environment_event(EnvironmentEvent::CheckDone(report));
            }
        });
    }

    // environment_snapshot / present_environment_dialog / refresh_environment_dialog /
    // environment_load_requested / environment_fix_requested / environment_recheck_requested
    // 六个成员按现实现原样迁入（spawn_permission_fix 为唯一改写体）。
}
```

- [ ] **Step 3: 跑模块测试**

Run: `cargo test -p magi-app 2>&1 | grep -E 'test result|error' | head -5`
Expected: PASS（environment/ui 相关锚点全绿；Task 1 的状态机测试此刻可跑）。

- [ ] **Step 4: 提交（与 Task 1 合并提交）**

```bash
git add -A crates/magi-app/src docs/specs/t7-magician
git commit -S -m "feat: 权限修复切换 setfacl 即时 ACL；环境编排独立成文件（D33 + D29）

- environment.rs：AutoAttempts 一次性纪律收进状态机；Ready 复发/复检失败转 Issue；未列组合总则；校验器与裁决器更名（username_is_allowed/sg_node_is_allowed/startup_verdict/recheck_verdict）；锚点改名 test_window_size_scales_with_display（代码+manifest+spec 三处同步）
- ui/environment_flow.rs：环境状态机宿主（事件入口/装载/修复/胶囊/向导接线）自 main_window 迁出；前置不满足不发起 pkexec 直接呈现手动指引"
```

---

### Task 4: UI 三页拆分（dashboard / diagnostics_page / about_page）

**Files:**
- Create: `crates/magi-app/src/ui/dashboard.rs`（仪表盘页控件集构建 + 设备分组呈现）
- Create: `crates/magi-app/src/ui/diagnostics_page.rs`（诊断页构建 + 刷新）
- Create: `crates/magi-app/src/ui/about_page.rs`（关于页构建 + 平台说明）
- Modify: `crates/magi-app/src/ui/main_window.rs`（imp::build 改调各页构建函数；`show_hit`/`show_unknown_device`/`refresh_diagnostics_view`/`show_platform_notice` 迁出；imp 页面字段重组为页结构体）
- Modify: `crates/magi-app/src/ui/mod.rs`

**Interfaces:**
- Produces（每页同构）: `pub(crate) struct DashboardWidgets { … }` + `pub(crate) fn build() -> (gtk::Widget, DashboardWidgets)`（返回页面根控件与控件集）；`impl MainWindow` 的呈现方法迁入各页文件。
- main_window.rs 保留：窗口骨架（headerbar/split_view/nav）、`start()` 装配、导航路由、作业与口令流程、`on_event`。

- [ ] **Step 1: dashboard.rs（模式样板，其余两页同构）**

从 imp::build 中把仪表盘段（设备图标/徽章/三信息行/操作五入口/进度与结果/toast_overlay 包裹）整体移入：

```rust
//! 仪表盘页（D29 分文件）：页面构建与设备分组呈现；编排仍经 MainWindow。

use adw::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use crate::device::jobs::ScanHit;
use crate::device::rescan::DeviceIdentity;
use crate::ui::MainWindow;

/// 仪表盘控件集（构建于本文件，装配进主窗口 imp）。
pub(crate) struct DashboardWidgets {
    pub toast_overlay: adw::ToastOverlay,
    pub device_group: adw::PreferencesGroup,
    pub device_row: adw::ActionRow,
    pub device_icon: gtk::Image,
    pub status_icon: gtk::Image,
    pub status_label: gtk::Label,
    pub badge_box: gtk::Box,
    pub node_row: adw::ActionRow,
    pub channel_row: adw::ActionRow,
    pub descriptor_row: adw::ActionRow,
    pub actions_group: adw::PreferencesGroup,
    pub action_unlock: adw::ButtonRow,
    pub action_validate_password: adw::ButtonRow,
    pub action_set_password: adw::ButtonRow,
    pub action_change_password: adw::ButtonRow,
    pub action_delete_password: adw::ButtonRow,
    pub feedback_group: adw::PreferencesGroup,
    pub progress_box: gtk::Box,
    pub progress: gtk::ProgressBar,
    pub action_cancel: gtk::Button,
    pub result_row: gtk::Box,
    pub result_icon: gtk::Image,
    pub result_label: gtk::Label,
}

/// 构建仪表盘页（返回页面根 = 包裹 PreferencesPage 的 ToastOverlay 与控件集）。
pub(crate) fn build() -> (gtk::Widget, DashboardWidgets) {
    // ……imp::build 中仪表盘段的构建代码原样移入（不改任何属性与层级）……
    // 结尾：
    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&page));
    (toast_overlay.clone().upcast(), DashboardWidgets { … })
}

impl MainWindow {
    /// 设备分组呈现（§4.11）：show_hit / show_unknown_device / set_badge_class
    /// 三个方法按现实现原样迁入本文件（引用改为 imp.dashboard.xxx）。
}
```

main_window.rs 侧：imp 字段替换为 `pub dashboard: dashboard::DashboardWidgets` 等；`build()` 内改 `let (dash_root, dashboard) = dashboard::build(); content_stack.add_named(&dash_root, Some(NavItem::Dashboard.page_name()));`；`show_hit`/`show_unknown_device`/`set_badge_class`/`apply_scan` 呈现段迁入 dashboard.rs（引用 `imp.dashboard.device_row` 等）；`action_row()`/`apply_actions` 留在 main_window（跨页编排），字段引用同步。

- [ ] **Step 2: diagnostics_page.rs / about_page.rs 同构迁移**

diagnostics_page.rs：`DiagnosticsWidgets { diagnostics_group, diagnostics_view, action_export_diagnostics }` + `build()` + `impl MainWindow { refresh_diagnostics_view }`。
about_page.rs：`AboutWidgets { about_group, about_version_row, about_device_row, about_repository_row, platform_group, platform_row }` + `build()` + `impl MainWindow { show_platform_notice }`。

- [ ] **Step 3: 全量测试**

Run: `cargo test -p magi-app 2>&1 | grep -E 'test result|error' | head -5`
Expected: PASS（`test_sidebar_navigation_items` 等结构断言经 imp 访问路径更新后全绿；锚点不动）。

- [ ] **Step 4: 提交**

```bash
git add -A crates/magi-app/src/ui
git commit -S -m "refactor: UI 三页拆分为 dashboard/diagnostics_page/about_page（D29 分文件组织）

每页=控件集结构体+构建函数+呈现方法（impl MainWindow 跨文件块）；
main_window 收敛为窗口骨架/装配/路由/作业与口令流程（约 -600 行）"
```

---

### Task 4b: WindowState 分域 + 外壳子结构体 + 接线归位（用户裁定「方案 A 治到底」）

**Files:**
- Modify: `crates/magi-app/src/ui/main_window.rs`（删 `WindowState`，imp 改持五个独立域 RefCell；外壳字段组 `HeaderBarWidgets`/`SidebarWidgets`；`prompt_password` 收敛）
- Create: 无新文件（域结构体定义在 main_window.rs 顶部，随域消费者跨文件可见）
- Modify: `crates/magi-app/src/ui/password_dialog.rs`（提交回调接线迁入）
- Modify: `crates/magi-app/src/settings.rs`（`apply_theme` 迁入）
- Modify: `crates/magi-app/src/ui/mod.rs`（删 `apply_theme`，只留模块声明）
- Modify: `crates/magi-app/src/lib.rs`（`connect_startup` 改调 `settings::apply_theme`）

**Interfaces:**
- Consumes: T3 的 `environment_flow.rs`（访问 `imp.env` 域）、T4 的页结构体（访问 `imp.replay`/`imp.job` 域）。
- Produces: imp 上的域字段（后续任务与测试的访问路径）：
  - `pub header: HeaderBarWidgets`、`pub sidebar: SidebarWidgets`（外壳）
  - `pub rescan: RefCell<RescanState>`、`pub job: RefCell<JobState>`、`pub replay: RefCell<ReplayState>`、`pub dialogs: RefCell<DialogRefs>`、`pub env: RefCell<EnvState>`

- [ ] **Step 1: 域结构体与 imp 重组（编译器驱动，行为零变化）**

删除 `WindowState` 与 `state()` 访问器，代之以：

```rust
/// 作业编排域：单飞闸门、在飞设备/动作与取消标志（不变量：device 与 action 同生命周期）。
pub(crate) struct JobState {
    pub gate: crate::device::gate::UnlockGate,
    pub device: Option<crate::device::jobs::DeviceJob>,
    pub cancel: crate::device::jobs::CancelFlag,
    pub action: Option<crate::device::gate::ActionId>,
    pub last_hit: Option<crate::device::jobs::ScanHit>,
    pub state: crate::device::environment::EnvironmentState,
    pub autos: crate::device::environment::AutoAttempts,
}

/// 外壳控件集：顶栏（窗口单元自有，非页面）。
pub(crate) struct HeaderBarWidgets {
    pub sidebar_toggle: gtk::ToggleButton,
    pub action_preferences: gtk::Button,
    pub environment_pill: gtk::Button,
    pub window_title: adw::WindowTitle,
}

/// 外壳控件集：侧边栏品牌与导航。
pub(crate) struct SidebarWidgets {
    pub title: gtk::Label,
    pub list: gtk::ListBox,
    pub labels: Vec<gtk::Label>,
}
```

全部 `self.state().borrow…` 访问点（约 30 处，含 environment_flow.rs / dashboard.rs 等已迁出文件）按域改写：环境域 `self.imp().env.borrow_mut().state/autos`、作业域 `self.imp().job.borrow_mut().gate/device/…`、重放域 `self.imp().replay.borrow_mut().last_hit/…`。测试中的 `imp.environment_pill` → `imp.header.environment_pill`、`imp.nav_list` → `imp.sidebar.list` 等同步。

- [ ] **Step 2: 接线归位**

1. `password_dialog.rs` 新增 `pub fn connect_submit(&self, f: impl Fn(magi_protocol::Password) + 'static)`：提交按钮点击、空口令就地提示、`take_password` 取值与 zeroize 语义全部在对话框内闭合；`main_window::prompt_password` 收敛为「复位闸门 → 建对话框 → 注册弱引用 → `connect_submit(start_job 闭包)`」四步（作业启动逻辑留 main_window）。
2. `apply_theme` 自 `ui/mod.rs` 移入 `settings.rs`（消费 `Settings` 的域）；`ui/mod.rs` 只留模块声明与导出；`lib.rs` 的 `connect_startup` 改 `settings::apply_theme(&settings)`。

- [ ] **Step 3: 全量测试（行为零变化验证）**

Run: `cargo test -p magi-app 2>&1 | grep -E 'test result|error' | head -5`
Expected: PASS，测试数量与 T4 完成时一致。

- [ ] **Step 4: 提交**

```bash
git add -A crates/magi-app/src
git commit -S -m "refactor: WindowState 分域 + 外壳子结构体 + 对话框接线归位（D29 到底）

- imp 改持五个独立域 RefCell（rescan/job/replay/dialogs/env），不变量各归其域，
  消除单结构八类混装与散布 30 处的整结构 borrow
- 顶栏/侧边栏组 HeaderBarWidgets/SidebarWidgets，imp 只剩外壳+页结构体+域
- 口令对话框提交流程（空口令提示/取值/zeroize）闭合进 password_dialog；
  apply_theme 归位 settings；main_window 收敛至 ~900 行纯外壳+路由"
```

---
### Task 5: jobs.rs 事件路由改窗口自持 Receiver

**Files:**
- Modify: `crates/magi-app/src/device/jobs.rs`、`crates/magi-app/src/ui/main_window.rs`

内容与上一版计划 Task 3 完全一致：删 `EVENT_SINK`/`set_event_sink`/`deliver`/`EventSink`；`spawn_device_job` 返回 `Result<async_channel::Receiver<AppEvent>, AppError>`；`start()` 删注册；`start_job` 消费自持 Receiver（`spawn_local` 循环 `this.on_event(event)`）。

- [ ] Steps: 改造 → `cargo test -p magi-app` PASS → 提交 `refactor: 设备作业事件改为窗口自持 Receiver，删除 thread_local 全局事件汇`（GPG）。

---

### Task 6: 启动异步化 + 周期环境自检

**Files:**
- Modify: `crates/magi-app/src/device/jobs.rs`、`crates/magi-app/src/ui/main_window.rs`

内容与上一版计划 Task 4 完全一致：`ScanOutcome` 加 `pub env: crate::device::environment::EnvironmentReport`；`fetch_scan` 两处构造补 `env: crate::device::environment::inspect_environment()`；`spawn_scan_watch` 循环改「先取数后休眠」；`start()` 删同步 `refresh_devices()`/`start_environment_bootstrap()`（后者函数已随 Task 3 迁移，直接删除）；`spawn_device_watch` 消费循环 `first` 标记首轮 authoritative + 每轮 `apply_environment_event(CheckDone(outcome.env.clone()))`。

- [ ] Steps: 改造 → `cargo test -p magi-app` PASS → 提交 `feat: 启动扫描与环境自检异步化，周期重扫每轮附带环境自检（§4.13/§6）`（GPG）。

---

### Task 7: 静态文案装配单点 + 去 expect_err

**Files:**
- Modify: `crates/magi-app/src/ui/main_window.rs`（`apply_static_texts`；`trigger()` 直接构造错误）

内容与上一版计划 Task 5/6 一致：抽 `fn apply_static_texts(&self)`（setup 与 relocalize 共用的全部静态文案行单点登记）；`trigger()` 写口令三分支合并为 `self.show_error(&AppError::Protocol(magi_protocol::ProtocolError::PasswordOperationUnspecified))`。

- [ ] Steps: 改造 → `cargo test -p magi-app` PASS → 提交 `refactor: 静态文案装配单点化；写口令入口直接构造错误值`（GPG）。

---

### Task 8: locales 修订

**Files:**
- Modify: `crates/magi-app/locales/zh-CN.yml`、`crates/magi-app/locales/en.yml`

内容与上一版计划 Task 7 完全一致：`platform.linux_notice` 改授权口径（旧文案「不请求管理员权限」为谎言）；`environment.issue_permission_advice` 改 setfacl 即时语义 + 手动命令 `sudo setfacl -m u:$USER:rw /dev/sg*`；两文件删空键 `issue:`。

- [ ] Steps: 改文案 → `cargo test -p magi-app test_locale_key_sets_are_equal` PASS → 提交 `fix: 平台与权限指引文案对齐 D30/D33 授权口径，删 issue 死键`（GPG）。

---

### Task 9: 全量验证 + spec 切 Authoritative

**Files:**
- Modify: `docs/specs/t7-magician/spec.md`、`docs/specs/t7-magician/tools/audit_manifest.json`

- [ ] **Step 1:** `cargo test --workspace`（全 ok 零 FAIL）+ `cargo clippy --workspace --all-targets`（error 计 0）。
- [ ] **Step 2:** manifest `spec_status` → `authoritative`；spec 头部 → `**状态:** Authoritative（唯一权威）`；§10 底部状态段重写（D30–D33 落地 + D1–D7 + 文件重组；测试数字以实跑为准；真机验证句待 Task 10 后补全或并入本段措辞「真机验证（D33）：产品路径修复→授权→ACL 即时生效→胶囊消失」）。
- [ ] **Step 3:** `cd docs/specs/t7-magician && python3 tools/test_audit_spec.py && python3 tools/audit_spec.py && python3 tools/barriers.py` → 17 OK / PASS 零 warning / 三屏障全绿。
- [ ] **Step 4:** 提交 `docs: spec 0.4 切回 Authoritative（D33 落地 + 文件重组，三件套全绿）`（GPG）。

---

### Task 10: 真机验证（需用户在场输 polkit 密码）

与上一版计划 Task 9 完全一致：重启应用（完整启动命令已给）→ 胶囊 → 引导向导 → 「修复设备权限」→ 授权 → 机器侧证据（`getfacl -p /dev/sg0 | grep 'user:'` 出现 `rw-`、`test -r -w` OK、胶囊消失、设备卡 `04e8:61fc` 已锁定）→ 可逆性抽验（可选）。

---

### Task 11: issues 登记收尾

**Files:**
- Create: `docs/specs/t7-magician/issues/2026-09-21-locales键风格统一.md`（UR15：11 个 PascalCase 呈现码键与功能键双风格混排，统一 `error.` 子树需动 presentation.rs 常量表与两份 locale）

（原「页面拆分」「controller 拆分」「WindowState 分域」三项已由 Task 2/4/4b 完成，不再登记。）

- [ ] Steps: 按 issues/README 骨架写文件（open 状态 + 证据行号 + 重构方向）→ 提交 `docs: 登记后续重构 issue（locales 键风格统一）`（GPG）。

---

## Self-Review 结论

1. **Spec 覆盖**：§4.13 全部行为 → T1/T3/T6；§6 NFR（首帧/提权）→ T3/T6；§4.11 文案口径 → T8；D29 组织约束 → T2/T3/T4；AC-016/017 验证 → T9/T10。缺口：无。
2. **占位符**：T3/T4 的「原样迁入/移入」均为既有代码搬迁指令（代码在仓库中，非新代码占位）；新增代码全部给全。
3. **类型一致性**：`AutoAttempts`（T1 产 / T3 用）、`permission_fix_commands(Option<&str>, &[String]) -> Option<Vec<String>>`、`ScanOutcome.env`（T6）、`spawn_device_job -> Result<Receiver<AppEvent>, AppError>`（T5 产用同任务）、`DashboardWidgets` 等页结构体（T4 产、main_window 用）——已核对一致。
