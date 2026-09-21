# D33 setfacl 切换与审计代码修订（D1–D7）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把已定稿的 spec 0.4（D33：`pkexec setfacl` 即时 ACL 权限修复）落进代码，并完成双审计提出的 7 项代码修订（D1–D7），最终全绿、真机验证、统一切回 Authoritative 并签名提交。

**Architecture:** 权限修复从「udev 规则脚本」（NixOS 上 `/etc/udev/rules.d` 指向只读 nix store，必然失败）切换为「argv 直传 `pkexec setfacl` 即时 ACL」。一次性自动修复纪律从 UI 层散装 bool 收进纯状态机（`AutoAttempts` 参数）。事件路由从 thread_local 全局汇改为窗口自持 Receiver；启动扫描与环境自检全部异步化。

**Tech Stack:** Rust 2021 / gtk4 0.11 + libadwaita 0.9 / async-channel / rust-i18n（zh-CN 默认 + en）/ cargo test / spec 自带审计三件套（python3）。

**Spec:** `docs/specs/t7-magician/spec.md`（版本 0.4，决策基线 D01–D33，当前 Draft——本计划完成后切回 Authoritative）。本计划实现 §4.13 REQ-013（D30/D33）、§4.12（D32 已落地部分不动）、§4.11 增补条款。

## Global Constraints

- 依赖方向单向：`magi-app` → `magi-protocol` → `magi-transport`；UI 不构造 CDB/令牌流（spec §3.1）。
- 用户可见文案一律经 i18n 键，代码零内联可显示字符串（AC-014/AC-015）；zh-CN 与 en 键集合一致（有测试断言）。
- workspace lints：`clippy correctness = deny`；`unsafe_op_in_unsafe_fn = deny`。
- 提权边界（§6）：除 `pkexec modprobe sg` 与 `pkexec setfacl`（argv 直传、`user`/`nodes` 白名单校验、不经 shell）外不请求提权；前置不满足不发起 pkexec，按修复失败呈现手动指引。
- 权限修复零持久化（D33）：不写规则文件、不改属主/用户组、不动节点 mode。
- 自动修复纪律（§4.13）：只在启动引导链（`Unknown`/`Checking`）内由状态机按 `AutoAttempts` 各至多一次；`Ready` 后复发与失败重试归用户；UI 层不得另设补偿标志。
- 周期重扫每轮附带环境自检（§4.13 验收）；环境自检与扫描不阻塞 UI 首帧（§6）。
- 提交纪律：中文 conventional commits，GPG 签名（`git commit -S`，密钥 `85E52EEE42578D11`）。
- 测试锚点（manifest 登记，必须真实存在）：`test_permission_fix_command_whitelist`、`test_environment_state_transitions`、`test_environment_report_missing_module`、`test_environment_report_permission_denied`、`test_environment_pill_present`、`test_window_size_scales_with_workarea`。
- 工作区现状：`crates/magi-app/src/environment.rs` 有编译断点（`spawn_permission_fix` 调用不存在的 `permission_fix_command()` 单数形式）与两个同名测试草稿——Task 1 一并清理。

---

### Task 1: environment.rs 状态机定稿（AutoAttempts + setfacl 命令 + 清理草稿）

**Files:**
- Modify: `crates/magi-app/src/environment.rs`（全文整理：保留 `inspect_environment`/`inspect_environment_in`；`permission_fix_commands`/`permission_fix_commands_with_path`/`current_user`/`user_is_safe`/`node_is_safe`/`resolve_setfacl` 已在文件中，删除残留的 udev 常量引用与两个同名草稿测试，重写测试区）

**Interfaces:**
- Produces（Task 2 消费）:
  - `pub struct AutoAttempts { pub module: bool, pub permissions: bool }`（`Debug/Clone/Copy/Default/PartialEq/Eq`）
  - `pub fn next_environment_state(state: EnvironmentState, event: EnvironmentEvent, autos: &mut AutoAttempts) -> (EnvironmentState, EnvironmentAction)`
  - `pub fn permission_fix_commands(user: Option<&str>, nodes: &[String]) -> Option<Vec<String>>`
  - `pub fn current_user() -> Option<String>`

- [ ] **Step 1: 重写测试区为失败态（先测后码）**

删除文件中现有的两个 `test_permission_fix_command_whitelist` 草稿与 `test_environment_state_transitions` 中引用旧签名 `next_environment_state(state, event)` 二参形式的全部调用，然后写入新测试（签名三参、`AutoAttempts` 语义）：

```rust
    /// 锚点（§10）：环境就绪状态机全表转换（含 AutoAttempts 一次性纪律，D33）。
    #[test]
    fn test_environment_state_transitions() {
        use EnvironmentAction as A;
        use EnvironmentEvent as E;
        use EnvironmentState as S;

        let missing = EnvironmentReport {
            issues: vec![EnvironmentIssue::SgModuleMissing],
        };
        let denied = EnvironmentReport {
            issues: vec![EnvironmentIssue::SgNodePermissionDenied {
                node: "/dev/sg0".to_string(),
            }],
        };
        let ready = EnvironmentReport::default();

        // 启动：空 → Ready + HidePill。
        let mut autos = AutoAttempts::default();
        let (s, a) = next_environment_state(S::Unknown, E::CheckDone(ready.clone()), &mut autos);
        assert_eq!((s, a), (S::Ready, A::HidePill));

        // 启动：模块缺失 → 自动装载（置位 autos.module）。
        let (s, a) = next_environment_state(S::Unknown, E::CheckDone(missing.clone()), &mut autos);
        assert_eq!(s, S::Loading(missing.issues.clone()));
        assert_eq!(a, A::LoadModule);
        assert!(autos.module && !autos.permissions);

        // 装载成功 → 复检轮；模块仍在（异常场景）→ 不再自动装载，转 Issue（防循环）。
        let (s, a) = next_environment_state(s, E::LoadSucceeded, &mut autos);
        assert_eq!((s, a), (S::Checking, A::None));
        let (s, a) = next_environment_state(s, E::CheckDone(missing.clone()), &mut autos);
        assert_eq!(s, S::Issue(missing.issues.clone()));
        assert_eq!(a, A::ShowPill);

        // 引导链串联：装载成功复检发现权限问题 → 自动修复（另一个标志位，各至多一次）。
        let mut autos = AutoAttempts::default();
        let (s, _) = next_environment_state(S::Unknown, E::CheckDone(missing.clone()), &mut autos);
        let (s, _) = next_environment_state(s, E::LoadSucceeded, &mut autos);
        let (s, a) = next_environment_state(s, E::CheckDone(denied.clone()), &mut autos);
        assert_eq!(s, S::Fixing(denied.issues.clone()));
        assert_eq!(a, A::FixPermissions);
        assert!(autos.module && autos.permissions);
        // 修复成功 → 复检 → 就绪。
        let (s, _) = next_environment_state(s, E::FixPermissionsSucceeded, &mut autos);
        let (s, a) = next_environment_state(s, E::CheckDone(ready.clone()), &mut autos);
        assert_eq!((s, a), (S::Ready, A::HidePill));

        // Ready 后问题复发 → Issue + ShowPill（不自动：重试归用户）。
        let (s, a) = next_environment_state(S::Ready, E::CheckDone(denied.clone()), &mut autos);
        assert_eq!(s, S::Issue(denied.issues.clone()));
        assert_eq!(a, A::ShowPill);

        // 用户显式重试不受 AutoAttempts 限制。
        let (s, a) = next_environment_state(s, E::LoadRequested, &mut autos);
        assert_eq!((s, a), (S::Loading(denied.issues.clone()), A::LoadModule));
        let (s, a) =
            next_environment_state(S::Issue(denied.issues.clone()), E::FixPermissionsRequested, &mut autos);
        assert_eq!((s, a), (S::Fixing(denied.issues.clone()), A::FixPermissions));

        // 失败分支：装载失败带回清单；修复失败回 Issue 呈现手动指引。
        let (s, a) = next_environment_state(S::Loading(missing.issues.clone()), E::LoadFailed, &mut autos);
        assert_eq!((s, a), (S::LoadFailed(missing.issues.clone()), A::ShowPill));
        let (s, a) =
            next_environment_state(S::Fixing(denied.issues.clone()), E::FixPermissionsFailed, &mut autos);
        assert_eq!((s, a), (S::Issue(denied.issues.clone()), A::ShowPill));

        // 单飞：在飞期间一切装载/修复请求被拒；装载与修复互斥并发。
        for state in [S::Loading(vec![]), S::Fixing(vec![])] {
            for event in [E::LoadRequested, E::FixPermissionsRequested] {
                let (s2, a2) = next_environment_state(state.clone(), event, &mut autos);
                assert_eq!((s2, a2), (state, A::None));
            }
        }

        // 总则：未列组合保持现状。
        let (s, a) = next_environment_state(S::Ready, E::LoadRequested, &mut autos);
        assert_eq!((s, a), (S::Ready, A::None));

        assert_eq!(module_load_command(), &["pkexec", "modprobe", "sg"]);
    }

    /// 锚点（§10）：权限修复命令固定形态（D33）——argv 直传、user/nodes 白名单。
    #[test]
    fn test_permission_fix_command_whitelist() {
        let dir = tempdir();
        fs::write(dir.path().join("setfacl"), b"#!/bin/sh\n").unwrap();
        let path = format!("{}:/usr/bin", dir.path().display());
        let nodes = vec!["/dev/sg0".to_string(), "/dev/sg1".to_string()];

        // 形态固定：pkexec <setfacl 绝对路径> -m u:<user>:rw <nodes…>。
        let argv = permission_fix_commands_with_path(Some("rikki"), &nodes, Some(&path))
            .expect("合法输入必须构造出命令");
        assert_eq!(argv[0], "pkexec");
        assert!(argv[1].ends_with("/setfacl"));
        assert_eq!(argv[2], "-m");
        assert_eq!(argv[3], "u:rikki:rw");
        assert_eq!(argv[4..], nodes);

        // 前置不满足 → None（§5：不发起 pkexec，按修复失败呈现手动指引）。
        assert!(permission_fix_commands_with_path(Some("rikki;rm"), &nodes, Some(&path)).is_none());
        assert!(permission_fix_commands_with_path(Some("a/b"), &nodes, Some(&path)).is_none());
        assert!(permission_fix_commands_with_path(None, &nodes, Some(&path)).is_none());
        assert!(permission_fix_commands_with_path(Some("rikki"), &[], Some(&path)).is_none());
        assert!(
            permission_fix_commands_with_path(Some("rikki"), &vec!["/dev/nvme0".to_string()], Some(&path))
                .is_none()
        );
        assert!(permission_fix_commands_with_path(Some("rikki"), &nodes, None).is_none());
        assert!(permission_fix_commands_with_path(Some("rikki"), &nodes, Some("/nonexistent")).is_none());

        // 校验器本身。
        assert!(user_is_safe("a.b-c_d") && node_is_safe("/dev/sg12"));
        assert!(!user_is_safe("") && !user_is_safe("rikki;rm -rf /"));
        assert!(!node_is_safe("/dev/sg") && !node_is_safe("/dev/sg0/../../etc") && !node_is_safe("/dev/nvme0"));
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p magi-app environment:: 2>&1 | tail -5`
Expected: 编译错误（`next_environment_state` 参数数量不符 / `AutoAttempts` 未定义）。

- [ ] **Step 3: 实现状态机与清理**

在 `environment.rs` 中：

1. `EnvironmentState` 枚举保持现有六个变体（`Unknown/Checking/Ready/Issue/Loading/Fixing/LoadFailed`，`Loading/Fixing/Issue/LoadFailed` 携带 `Vec<EnvironmentIssue>`）。
2. 新增 `AutoAttempts`（`#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]`，两个 `pub bool` 字段 `module`/`permissions`）。
3. 迁移函数改为三参与分支重写（替换现有实现与 `startup_done`）：

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
        // 启动引导链（Unknown/Checking）：按 AutoAttempts 各至多自动一次。
        (S::Unknown | S::Checking, E::CheckDone(report)) => startup_done(report, autos),
        // 装载收尾：成功 → 复检轮；失败 → 带回问题清单呈现。
        (S::Loading(_), E::LoadSucceeded) => (S::Checking, A::None),
        (S::Loading(issues), E::LoadFailed) => (S::LoadFailed(issues), A::ShowPill),
        // 单飞：在飞期间拒绝并发装载与修复。
        (S::Loading(issues), E::LoadRequested | E::FixPermissionsRequested) => {
            (S::Loading(issues), A::None)
        }
        // 权限修复收尾：成功 → 复检轮；失败 → 回 Issue 呈现手动指引（D33）。
        (S::Fixing(_), E::FixPermissionsSucceeded) => (S::Checking, A::None),
        (S::Fixing(issues), E::FixPermissionsFailed) => (S::Issue(issues), A::ShowPill),
        (S::Fixing(issues), E::FixPermissionsRequested | E::LoadRequested) => {
            (S::Fixing(issues), A::None)
        }
        // 用户显式重试（不受 AutoAttempts 限制）。
        (S::Issue(issues) | S::LoadFailed(issues), E::LoadRequested) => {
            (S::Loading(issues), A::LoadModule)
        }
        (S::Issue(issues) | S::LoadFailed(issues), E::FixPermissionsRequested) => {
            (S::Fixing(issues), A::FixPermissions)
        }
        // 复检（含 Ready 后复发）：只刷新呈现，不自动修复。
        (S::Issue(_) | S::LoadFailed(_) | S::Ready, E::CheckDone(report)) => recheck_done(report),
        // 总则：未列组合保持现状。
        (state, _) => (state, A::None),
    }
}

/// 启动引导链的裁决：自动装载/修复各至多一次（AutoAttempts 置位），循环由构造排除。
fn startup_done(report: EnvironmentReport, autos: &mut AutoAttempts) -> (EnvironmentState, EnvironmentAction) {
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

4. `recheck_done` 保持现状（空 → `Ready`+`HidePill`；非空 → `Issue`+`ShowPill`）。
5. 删除文件中所有 udev 残留（`UDEV_RULE_PATH`/`PERMISSION_FIX_SCRIPT` 等常量若仍存在）与重复草稿测试；确认 `permission_fix_commands`（读进程 `PATH`）只是 `permission_fix_commands_with_path(user, nodes, std::env::var("PATH").ok().as_deref())` 的包装。

- [ ] **Step 4: 运行测试通过**

Run: `cargo test -p magi-app environment:: 2>&1 | tail -5`
Expected: environment 模块全部测试 PASS（此时 `main_window.rs` 仍编译不过，属预期——Task 2 修复；如需单看本模块可 `cargo test -p magi-app --lib environment::`，若整体编译不过则先完成 Task 2 Step 3 再回来跑）。

- [ ] **Step 5: 提交（与 Task 2 合并提交，见 Task 2 Step 5）**

---

### Task 2: main_window.rs 环境接线切换（AutoAttempts 落地 + setfacl 命令 + None→手动指引）

**Files:**
- Modify: `crates/magi-app/src/ui/main_window.rs`（`WindowState`、`apply_environment_event`、`spawn_permission_fix`）

**Interfaces:**
- Consumes: Task 1 的 `AutoAttempts`、`next_environment_state` 三参签名、`permission_fix_commands`、`current_user`、`EnvironmentIssue`。

- [ ] **Step 1: WindowState 换标志**

`WindowState` 中删除 `auto_load_used: bool` 与 `auto_fix_used: bool` 两个字段，新增：

```rust
    /// 环境自动修复一次性纪律（§4.13：由状态机消费，UI 不得另设补偿标志）。
    env_autos: environment::AutoAttempts,
```

- [ ] **Step 2: apply_environment_event 直通状态机**

替换 `apply_environment_event` 全函数（删除 `is_user_retry` 与 match 守卫块——一次性纪律已由状态机保证）：

```rust
    /// 环境状态机入口（§4.13）：纯迁移 + 动作落位（胶囊显隐 / 工作线程装载与修复）。
    fn apply_environment_event(&self, event: EnvironmentEvent) {
        let (next, action) = {
            let mut state = self.state().borrow_mut();
            let (next, action) =
                next_environment_state(state.environment.clone(), event, &mut state.env_autos);
            state.environment = next.clone();
            (next, action)
        };
        let _ = next;
        match action {
            EnvironmentAction::None => {}
            EnvironmentAction::ShowPill => self.imp().environment_pill.set_visible(true),
            EnvironmentAction::HidePill => self.imp().environment_pill.set_visible(false),
            EnvironmentAction::LoadModule => self.spawn_module_load(),
            EnvironmentAction::FixPermissions => self.spawn_permission_fix(),
        }
        self.refresh_environment_dialog();
    }
```

- [ ] **Step 3: spawn_permission_fix 切 setfacl**

替换 `spawn_permission_fix` 全函数（旧版调用不存在的 `environment::permission_fix_command()` 并带 udev 注释）：

```rust
    /// 权限修复（§4.13，D33）：`pkexec setfacl -m u:<user>:rw <nodes…>` 即时 ACL，
    /// argv 直传（不经 shell）、user/nodes 白名单校验、零持久化；在工作线程执行，
    /// 单飞由状态机保证（Fixing 态拒绝并发）；结果回主线程复检一轮。
    fn spawn_permission_fix(&self) {
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
        // 前置不满足（无法确定用户 / setfacl 不可用 / 节点为空）→ 不发起 pkexec，
        // 按修复失败呈现手动指引（§5「权限修复前置不满足」）。
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
                // 收尾后统一复检：ACL 生效后节点可读写 → Ready。
                let report = environment::inspect_environment();
                this.apply_environment_event(EnvironmentEvent::CheckDone(report));
            }
        });
    }
```

- [ ] **Step 4: 编译并跑模块测试**

Run: `cargo test -p magi-app 2>&1 | grep -E 'test result|error' | head -5`
Expected: 全部 PASS（49+ 上一基线 + 本计划新增断言均绿；`spawn_module_load` 路径不变）。

- [ ] **Step 5: 提交**

```bash
git add crates/magi-app/src/environment.rs crates/magi-app/src/ui/main_window.rs
git commit -S -m "feat: 权限修复切换 setfacl 即时 ACL，一次性纪律收进状态机（D33）

- environment.rs：next_environment_state 增加 AutoAttempts 参数，启动引导链内装载/修复各至多自动一次，Ready 后复发与复检失败转 Issue 由用户重试；Ready+CheckDone(非空) 与未列组合总则落位；清理 udev 残留与重复草稿测试
- main_window.rs：spawn_permission_fix 改用 permission_fix_commands（argv 直传 pkexec setfacl，user/nodes 白名单），前置不满足不发起 pkexec 直接按失败呈现手动指引；删除 UI 层 auto_load_used/auto_fix_used 补偿标志"
```

---

### Task 3: jobs.rs 事件路由改窗口自持 Receiver（删 thread_local 全局汇）

**Files:**
- Modify: `crates/magi-app/src/jobs.rs`（删 `EVENT_SINK`/`set_event_sink`/`deliver`；`spawn_device_job` 改返回 `Receiver<AppEvent>`）
- Modify: `crates/magi-app/src/ui/main_window.rs`（`start()` 删 `set_event_sink` 注册；新增作业事件消费）

**Interfaces:**
- Produces: `pub fn spawn_device_job<F>(dev: DeviceId, job: F) -> Result<async_channel::Receiver<AppEvent>, AppError>`（Task 4/后续在 main_window 消费）。
- 删除符号：`set_event_sink`（全仓唯一调用点 main_window.rs:586 一并删除）。

- [ ] **Step 1: jobs.rs 删全局汇、spawn_device_job 返回 Receiver**

删除 `thread_local!` 块、`EventSink` 类型别名、`set_event_sink`、`deliver` 四段；`spawn_device_job` 改为：

```rust
/// §4.13：在工作线程执行 `job`，返回事件通道由调用方（主窗口）自持自消费；
/// 每设备单飞，已有在飞任务时返回 `AppError::Busy`。
pub fn spawn_device_job<F>(
    dev: DeviceId,
    job: F,
) -> Result<async_channel::Receiver<AppEvent>, AppError>
where
    F: FnOnce(&dyn Fn(AppEvent)) -> Result<Option<UnlockEvidence>, AppError> + Send + 'static,
{
    let (receiver, _handle) = run_device_job(dev, job)?;
    Ok(receiver)
}
```

- [ ] **Step 2: main_window 消费自己的 Receiver**

`start()` 删除这两行：

```rust
        let this = self.clone();
        jobs::set_event_sink(move |event| this.on_event(event));
```

`start_job`（main_window.rs:1259 附近 `let spawned = jobs::spawn_device_job(...)`）改为消费 Receiver：

```rust
        let receiver = jobs::spawn_device_job(job.device.clone(), move |emit| match action {
            // ……原有 match 体不动……
        });
        match receiver {
            Ok(receiver) => {
                let this = self.clone();
                glib::MainContext::default().spawn_local(async move {
                    while let Ok(event) = receiver.recv().await {
                        this.on_event(event);
                    }
                });
            }
            Err(error) => self.show_error(&error),
        }
```

（若原实现用 `if let`/`match` 形态不同，保持原错误处理分支语义，只把 `Ok(())` 改 `Ok(receiver)` 并加消费循环。）

- [ ] **Step 3: 跑既有测试**

Run: `cargo test -p magi-app 2>&1 | grep -E 'test result|error' | head -5`
Expected: PASS（jobs.rs 内如有引用 `set_event_sink` 的测试同步删除或改写为 Receiver 形态）。

- [ ] **Step 4: 提交**

```bash
git add crates/magi-app/src/jobs.rs crates/magi-app/src/ui/main_window.rs
git commit -S -m "refactor: 设备作业事件改为窗口自持 Receiver，删除 thread_local 全局事件汇

修复二次 activate 后新窗口作业事件路由到旧窗口闭包的缺陷；全局一次性注册被静默忽略的隐患一并消除"
```

---

### Task 4: 启动异步化 + 周期重扫附带环境自检

**Files:**
- Modify: `crates/magi-app/src/jobs.rs`（`ScanOutcome` 加 `env` 字段；`fetch_scan` 顺带自检；`spawn_scan_watch` 首轮立即）
- Modify: `crates/magi-app/src/ui/main_window.rs`（`start()` 删同步 `refresh_devices()`/`start_environment_bootstrap()`；`spawn_device_watch` 消费 loop 首轮 authoritative 并投递环境事件；删除 `start_environment_bootstrap`）

**Interfaces:**
- Consumes: Task 1 的 `EnvironmentReport`/`inspect_environment`；Task 3 后的 Receiver 模式不涉及。
- Produces: `ScanOutcome { hits, error, env: crate::environment::EnvironmentReport }`（`env` 参与周期投递）。

- [ ] **Step 1: jobs.rs 取数段附带环境自检**

```rust
/// 一轮设备扫描的结果（取数段输出、呈现段输入；跨线程投递给主线程）。
#[derive(Debug, Default)]
pub struct ScanOutcome {
    /// 命中的设备（已裁剪到同时受理上限，§6）。
    pub hits: Vec<ScanHit>,
    /// 扫描失败（呈现段裁决是否打扰用户：权威轮次呈现，周期轮次只记诊断）。
    pub error: Option<AppError>,
    /// 同轮环境自检结果（§4.13：周期重扫每轮附带，主线程转 CheckDone 事件）。
    pub env: crate::environment::EnvironmentReport,
}
```

`fetch_scan` 两处构造补 `env: crate::environment::inspect_environment()`；`spawn_scan_watch` 线程循环改为「先取数后休眠」（首轮立即，启动不阻塞首帧）：

```rust
        .spawn(move || loop {
            // 首轮立即：启动扫描走本线程，主线程只装配（§6 不阻塞首帧）。
            if sender.send_blocking(fetch_scan()).is_err() {
                break;
            }
            thread::sleep(RESCAN_INTERVAL);
        });
```

- [ ] **Step 2: main_window 启动路径改异步 + 环境事件投递**

`start()` 删除 `self.start_environment_bootstrap();` 与 `self.refresh_devices();` 两行（保留 `connect_actions/refresh_actions/spawn_device_watch`）；删除 `start_environment_bootstrap` 整个函数。`spawn_device_watch` 的消费循环改为：

```rust
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let mut first = true;
            while let Ok(outcome) = receiver.recv().await {
                if let Some(error) = &outcome.error {
                    diagnostics::ring().record(Level::Warn, presentation::presentation_code(error));
                }
                // 周期重扫附带环境自检（§4.13）：每轮转 CheckDone，状态机裁决胶囊与修复。
                this.apply_environment_event(presentation_free_check_done(&outcome.env));
                this.apply_scan(&outcome, first);
                first = false;
            }
        });
```

其中 `presentation_free_check_done` 直接内联为：

```rust
                this.apply_environment_event(crate::environment::EnvironmentEvent::CheckDone(
                    outcome.env.clone(),
                ));
```

（`EnvironmentReport` 需 `Clone`——Task 1 已有 derive；若 `spawn_device_watch` 原实现里 `apply_scan(&outcome, false)` 固定 false，按上面 `first` 逻辑改：首轮 authoritative=true 对齐原 `refresh_devices` 语义。`refresh_devices()` 函数保留——会话收尾路径 main_window.rs:1323 仍在用，其内部追加一行 `self.apply_environment_event(...CheckDone(inspect_environment()))` 不需要：会话收尾不改环境。）

- [ ] **Step 3: 跑测试**

Run: `cargo test -p magi-app 2>&1 | grep -E 'test result|error' | head -5`
Expected: PASS（jobs.rs 中构造 `ScanOutcome` 的测试补 `..Default::default()` 或 `env: EnvironmentReport::default()`）。

- [ ] **Step 4: 提交**

```bash
git add crates/magi-app/src/jobs.rs crates/magi-app/src/ui/main_window.rs
git commit -S -m "feat: 启动扫描与环境自检异步化，周期重扫每轮附带环境自检（§4.13/§6）

- spawn_scan_watch 首轮立即取数，activate 只装配回调，不再同步扫 sysfs 阻塞首帧
- ScanOutcome 携带同轮 EnvironmentReport，主线程转 CheckDone 事件驱动胶囊/修复状态机"
```

---

### Task 5: setup/relocalize 文案装配去重

**Files:**
- Modify: `crates/magi-app/src/ui/main_window.rs`（抽 `apply_static_texts`）

- [ ] **Step 1: 抽公共函数**

`setup()` 与 `relocalize()` 中重复的静态文案装配段（侧边栏标题/导航标签/四个分组标题/五入口/胶囊/取消/首选项/侧边栏开关提示等，两处约 50 行逐行相同）抽为一个函数，两处调用：

```rust
    /// 静态文案装配单点（setup 与 relocalize 共用；新增控件只在此登记一次，
    /// 语言切换后无旧语言残留——D28 黑盒判据）。
    fn apply_static_texts(&self) {
        let imp = self.imp();
        imp.sidebar_title.set_label(&t!("app.title"));
        // ……把 setup() 中从 sidebar_title 到 sidebar_toggle.set_tooltip_text 的
        // 全部静态文案行原样移入（不改动任何键名与控件）……
        imp.environment_pill.set_label(&t!("environment.pill_label"));
        imp.environment_pill.set_tooltip_text(Some(&t!("environment.pill_tooltip")));
    }
```

`setup()` 保留结构装配与初始选中态，文案部分改为 `self.apply_static_texts();`；`relocalize()` 开头调用 `self.apply_static_texts();`，其后保留动态重放（设备分组/结果区/诊断视图/对话框）。

- [ ] **Step 2: 跑测试**

Run: `cargo test -p magi-app 2>&1 | grep -E 'test result|error' | head -3`
Expected: PASS（含 `test_sidebar_navigation_items` 等依赖文案键的测试）。

- [ ] **Step 3: 提交**

```bash
git add crates/magi-app/src/ui/main_window.rs
git commit -S -m "refactor: setup/relocalize 静态文案装配收敛为 apply_static_texts 单点

消除双份 ~50 行重复：新增控件漏登记一处即语言切换残留旧语言（D28 黑盒判据的隐患面）"
```

---

### Task 6: 写口令入口去 expect_err 运行时断言

**Files:**
- Modify: `crates/magi-app/src/ui/main_window.rs`（`trigger()`）

- [ ] **Step 1: 直接构造错误**

`trigger()` 中三个写口令分支合并为直接构造（消除运行时 `expect_err` panic 面）：

```rust
            // §4.11：写口令入口受证据缺口约束；程序化触发也必须立即返回、不下发任何命令。
            ActionId::SetPassword | ActionId::ChangePassword | ActionId::DeletePassword => {
                self.show_error(&AppError::Protocol(
                    magi_protocol::ProtocolError::PasswordOperationUnspecified,
                ));
            }
```

（若 `ProtocolError` 路径不同——以 presentation.rs 中 `From<ProtocolError>` 使用的导入为准对齐。）

- [ ] **Step 2: 跑测试并提交**

Run: `cargo test -p magi-app 2>&1 | grep -E 'test result|error' | head -3` → PASS

```bash
git add crates/magi-app/src/ui/main_window.rs
git commit -S -m "refactor: 写口令入口直接构造 PasswordOperationUnspecified，去掉 expect_err 运行时断言"
```

---

### Task 7: locales 修订（口径修正 + 删死键）

**Files:**
- Modify: `crates/magi-app/locales/zh-CN.yml`、`crates/magi-app/locales/en.yml`

- [ ] **Step 1: 替换两键 + 删死键**

zh-CN.yml：

```yaml
platform:
  linux_notice: "本工具通过 Linux 的 SCSI 通用接口与设备通信。运行环境缺失（内核模块、设备权限）时，会弹出的系统授权窗口向你索要管理员密码进行修复；也可以按引导向导中的手动指引自行处理。"
```

`environment.issue_permission_advice` 改为：

```yaml
  issue_permission_advice: "点击「修复设备权限」立即授予本设备读写（需管理员密码；不落任何系统配置，重插或重启后如再受限，重新修复即可）；也可手动执行 sudo setfacl -m u:$USER:rw /dev/sg*。"
```

en.yml 对应：

```yaml
platform:
  linux_notice: "This tool talks to the drive through Linux's SCSI generic interface. When the runtime environment is missing (kernel module, device permissions), a system authorization window will ask for the administrator password to fix it; you can also follow the manual steps in the setup guide."
```

```yaml
  issue_permission_advice: "Click \"Fix device permissions\" to grant read/write on this drive immediately (administrator password required; nothing is written to system configuration — if access is lost after replug or reboot, just fix it again), or run sudo setfacl -m u:$USER:rw /dev/sg* manually."
```

两份文件删除空键行 `issue:`（及其后空行）。

- [ ] **Step 2: 跑键集一致测试**

Run: `cargo test -p magi-app test_locale_key_sets_are_equal test_message_keys_resolve 2>&1 | tail -3`
Expected: PASS（zh/en 键集合一致，全部键可解析）。

- [ ] **Step 3: 提交**

```bash
git add crates/magi-app/locales/zh-CN.yml crates/magi-app/locales/en.yml
git commit -S -m "fix: 平台与权限指引文案对齐 D30/D33 授权口径，删除 udev 残留描述与 issue 死键"
```

---

### Task 8: 全量验证 + spec 三方统一切回 Authoritative

**Files:**
- Modify: `docs/specs/t7-magician/spec.md`（头部状态行、§10 底部状态段）
- Modify: `docs/specs/t7-magician/tools/audit_manifest.json`（`spec_status`）

- [ ] **Step 1: 全量测试与 clippy**

Run: `cargo test --workspace 2>&1 | grep -E 'test result' | head -8` → 全部 ok、零 FAILED
Run: `cargo clippy --workspace --all-targets 2>&1 | grep -cE '^error'` → 0（magi-transport 的既有 unused import 警告不属于本计划，忽略）

- [ ] **Step 2: 三方统一**

manifest: `"spec_status": "draft"` → `"authoritative"`。
spec 头部第 3 行 → `**状态:** Authoritative（唯一权威）`。
§10 底部状态段替换为（测试数字以 Step 1 实跑结果为准填入）：

```markdown
**当前状态（Authoritative）**：D30–D33 全部落地。D33（权限修复 `pkexec setfacl` 即时 ACL、零持久化、`AutoAttempts` 一次性纪律）与双审计修订（spec 正文 15 项 + 代码 D1–D7：locales 口径、事件路由窗口自持、启动异步化与周期环境自检、文案装配单点、去 expect_err）已随代码验证：<日期> 实跑 `cargo test --workspace` 全绿（magi-app <N>、magi-protocol 57、magi-transport 25）；`python3 tools/test_audit_spec.py` 17 例通过；`python3 tools/audit_spec.py` PASS 且零 warning；`python3 tools/barriers.py` 三条屏障全绿、退出码 0。真机验证（D33）：产品路径「修复设备权限」→ polkit 授权 → `setfacl` 即时生效（ACL `user:<user>:rw-`）→ 复检就绪 → 胶囊消失。本文件为唯一权威规格：行为变更必须先在 §9 决策日志新增或归因决策 ID 并同步 `tools/audit_manifest.json`，全部验证 PASS 后再改代码，spec 与代码同批提交。
```

- [ ] **Step 3: 审计三件套**

Run: `cd docs/specs/t7-magician && python3 tools/test_audit_spec.py && python3 tools/audit_spec.py && python3 tools/barriers.py`
Expected: 17 tests OK / PASS 零 warning / 三屏障全绿。

- [ ] **Step 4: 提交（spec 与代码同批收尾）**

```bash
git add docs/specs/t7-magician/spec.md docs/specs/t7-magician/tools/audit_manifest.json
git commit -S -m "docs: spec 0.4 切回 Authoritative（D33 落地，三件套全绿）"
```

---

### Task 9: 真机验证（产品路径端到端）

**Files:** 无代码改动（验证任务）

- [ ] **Step 1: 重启应用**

```bash
pkill -f 'target/debug/magi' || true; sleep 1
cargo build -p magi-app
NOHUP=$(type -P nohup); env -i WAYLAND_DISPLAY=wayland-0 XDG_RUNTIME_DIR=/run/user/1000 \
  DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus HOME=/home/rikki \
  PATH=/run/wrappers/bin:/nix/var/nix/profiles/default/bin:/usr/bin:/bin \
  "$NOHUP" ./target/debug/magi >/tmp/magi-smoke.log 2>&1 &
sleep 5 && pgrep -f 'target/debug/magi'
```

- [ ] **Step 2: 人工交互（需要用户在场输密码）**

应用窗口：右上角黄色胶囊 → 点击 → 引导向导 → 「修复设备权限」→ polkit 弹窗输密码。

- [ ] **Step 3: 机器侧证据**

```bash
getfacl -p /dev/sg0 | grep 'user:'   # 期望出现 user:<user>:rw-
test -r /dev/sg0 -a -w /dev/sg0 && echo "RW OK"
pgrep -f pkexec || echo "pkexec 已收尾"
```

期望：ACL 生效、RW OK、胶囊消失、设备卡仍显示 `04e8:61fc` 已锁定、结果区无 PermissionDenied。

- [ ] **Step 4: 可逆性抽验（可选，需再次授权）**

```bash
pkexec /run/current-system/sw/bin/setfacl -x u:$(id -un) /dev/sg0 && getfacl -p /dev/sg0 | grep -c "user:$(id -un)" || echo "已还原"
```

---

### Task 10: issues 登记后续重构项 + 收尾

**Files:**
- Create: `docs/specs/t7-magician/issues/2026-09-21-主窗口页面级拆分.md`
- Create: `docs/specs/t7-magician/issues/2026-09-21-controller模块拆分.md`
- Create: `docs/specs/t7-magician/issues/2026-09-21-WindowState分域.md`
- Create: `docs/specs/t7-magician/issues/2026-09-21-locales键风格统一.md`

- [ ] **Step 1: 按 issues/README.md 骨架写四个文件**

每个文件登记：问题（对应审计 UR9/UR13/UR14/UR15 的证据行号）、状态 `open`、建议重构方向（已在审计报告给出）、关联决策（D29 组织约束 / 无）。

- [ ] **Step 2: 提交**

```bash
git add docs/specs/t7-magician/issues/
git commit -S -m "docs: 登记四项后续重构 issue（页面拆分/controller/WindowState/locales 键风格）"
```

---

## Self-Review 结论

1. **Spec 覆盖**：§4.13 REQ-013（D30/D33 契约、转换表、纪律）→ Task 1/2/4；§4.12 D32 呈现码已落地不动；§4.11 增补（胶囊/向导/尺寸/组织约束）→ 已落地部分不动，文案口径 → Task 7；§6 NFR（不阻塞首帧、提权边界）→ Task 4/2；AC-016/017 → Task 8 验证口径。缺口：无。
2. **占位符扫描**：Task 5 Step 1 的「……原样移入……」是对既有 50 行的搬移指令（代码已在文件中，非新代码占位）；其余步骤均含完整代码。
3. **类型一致性**：`AutoAttempts`（Task 1 定义 / Task 2 使用）、`permission_fix_commands(Option<&str>, &[String]) -> Option<Vec<String>>`（Task 1/2 一致）、`ScanOutcome.env: EnvironmentReport`（Task 4 内部一致）、`spawn_device_job -> Result<Receiver<AppEvent>, AppError>`（Task 3 定义并消费）——已核对。
