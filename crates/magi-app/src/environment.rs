//! §4.13 环境就绪引导（D30）：自检、模块装载状态机（纯逻辑，无 GTK）。
//!
//! K3 同纪律：只读世界可读文件，不提权。所有路径由调用方以「根目录」形式注入
//! （[`inspect_environment_in`]），判定逻辑可在任意平台上用构造出来的假 sysfs/devfs
//! 树做真测试。
//!
//! 环境就绪态 `EnvironmentState` 是应用外壳态（§3.2 分层说明：不进协议状态模型），
//! 与设备态、会话态正交——未就绪时设备扫描照常进行（预期空态），由胶囊与引导向导
//! 呈现问题。

use std::fs;
use std::path::{Path, PathBuf};

/// sg 模块在 sysfs 中的标记目录（相对注入根）。
const SG_MODULE_DIR: &str = "module/sg";

/// 设备节点名前缀（`dev_root` 下的 `sgN`）。
const SG_NODE_PREFIX: &str = "sg";

/// 环境问题（事实层，不是 `AppError`；消费方是环境胶囊与引导向导，§5）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentIssue {
    /// `/sys/module/sg` 不存在：`sg` 内核模块未装载。
    SgModuleMissing,
    /// 设备节点存在但当前用户不可读写（提前于 open 的 EACCES 判定）。
    SgNodePermissionDenied {
        /// 不可读写的节点路径（注入根下的形式）。
        node: String,
    },
}

/// 环境自检报告（问题清单为空即环境就绪）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvironmentReport {
    /// 发现的问题（去重后的确定性顺序）。
    pub issues: Vec<EnvironmentIssue>,
}

/// 环境自检（默认真实根目录）。
pub fn inspect_environment() -> EnvironmentReport {
    inspect_environment_in(Path::new("/sys"), Path::new("/dev"))
}

/// [`inspect_environment`] 的可注入版本：`sys_root` 下查 `module/sg`，`sg` 模块已装载时
/// 再在 `dev_root` 下逐个探测 `sgN` 节点的读写权限。
///
/// 节点探测用真实 open（读写）——与 §5 `TransportError::PermissionDenied` 同一判定源，
/// 比模式位/属主推断更准（覆盖属组与 ACL）；sg 字符设备 open 本身不下发任何命令。
pub fn inspect_environment_in(sys_root: &Path, dev_root: &Path) -> EnvironmentReport {
    let mut issues = Vec::new();
    if !sys_root.join(SG_MODULE_DIR).is_dir() {
        issues.push(EnvironmentIssue::SgModuleMissing);
        // 模块未装载时不可能有 sg 节点，直接返回。
        return EnvironmentReport { issues };
    }
    for node in sg_nodes(dev_root) {
        let text = node.to_string_lossy().into_owned();
        match fs::OpenOptions::new().read(true).write(true).open(&node) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                issues.push(EnvironmentIssue::SgNodePermissionDenied { node: text });
            }
            // 其它错误（节点消失竞态等）留给 open 路径按 §5 呈现，不预判。
            Err(_) => {}
        }
    }
    EnvironmentReport { issues }
}
/// 用户名白名单字符（字母数字与 `_`/`-`/`.`）：ACL 命令以 argv 直传 `pkexec`，
/// 不经 shell；校验是为防环境变量被构造出形如路径穿越或参数注入的值。
fn user_is_safe(user: &str) -> bool {
    !user.is_empty()
        && user
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

/// 设备节点白名单：`/dev/sg` + 纯数字（来源即 sysfs 扫描，双重校验）。
fn node_is_safe(node: &str) -> bool {
    node.strip_prefix("/dev/sg")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()))
}

/// 在 `PATH` 中解析 `setfacl` 的绝对路径（pkexec 不继承调用方 PATH 语义，
/// 绝对路径最稳；找不到返回 `None` 走手动指引）。
fn resolve_setfacl(path_var: Option<&str>) -> Option<String> {
    let path_var = path_var?;
    path_var.split(':').find_map(|dir| {
        let candidate = std::path::Path::new(dir).join("setfacl");
        candidate.is_file().then(|| candidate.to_string_lossy().into_owned())
    })
}

/// 模块装载的固定命令（白名单出口：不得拼接任何用户输入；经 polkit 图形授权）。
pub fn module_load_command() -> &'static [&'static str] {
    &["pkexec", "modprobe", "sg"]
}

/// 当前用户名（`USER` 优先，回落 `LOGNAME`；均缺失或未过白名单返回 `None`）。
pub fn current_user() -> Option<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()
        .filter(|user| user_is_safe(user))
}

/// 权限修复命令（D33：`pkexec setfacl -m u:<user>:rw <nodes…>` 即时 ACL，
/// 零持久化）。argv 直传（不经 shell）；`user`/`nodes` 过白名单、`setfacl`
/// 以绝对路径解析。任一环节不满足返回 `None`（调用方呈现手动指引）。
pub fn permission_fix_commands(
    user: Option<&str>,
    nodes: &[String],
) -> Option<Vec<String>> {
    permission_fix_commands_with_path(user, nodes, std::env::var("PATH").ok().as_deref())
}

/// [`permission_fix_commands`] 的可注入版本（`path_var` 用于解析 `setfacl`）。
pub fn permission_fix_commands_with_path(
    user: Option<&str>,
    nodes: &[String],
    path_var: Option<&str>,
) -> Option<Vec<String>> {
    let user = user.filter(|user| user_is_safe(user))?;
    let setfacl = resolve_setfacl(path_var)?;
    let mut argv = vec![
        "pkexec".to_string(),
        setfacl,
        "-m".to_string(),
        format!("u:{user}:rw"),
    ];
    for node in nodes {
        node_is_safe(node).then_some(())?;
        argv.push(node.clone());
    }
    (argv.len() > 4).then_some(argv)
}
/// 环境就绪态（D30；应用外壳态）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum EnvironmentState {
    /// 尚未执行自检。
    #[default]
    Unknown,
    /// 自检进行中（含装载成功后的复检轮次）。
    Checking,
    /// 环境就绪：`sg` 模块已装载，设备节点（若存在）当前用户可读写。
    Ready,
    /// 自检发现问题；问题清单随状态携带。
    Issue(Vec<EnvironmentIssue>),
    /// 模块装载在飞（单飞：同时最多一次）；括号内为触发本次装载的问题清单，
    /// `LoadFailed` 时原样带回。
    Loading(Vec<EnvironmentIssue>),
    /// 装载被拒或失败；问题清单随状态携带。
    LoadFailed(Vec<EnvironmentIssue>),
    /// 权限修复在飞（单飞：同时最多一次）；括号内为触发本次修复的问题清单，
    /// 修复失败时原样带回（D33）。
    Fixing(Vec<EnvironmentIssue>),
}

/// 环境就绪态迁移事件。
#[derive(Debug, Clone)]
pub enum EnvironmentEvent {
    /// 一轮自检完成（启动或复检）。
    CheckDone(EnvironmentReport),
    /// 用户在引导向导中显式请求装载。
    LoadRequested,
    /// 装载命令成功退出。
    LoadSucceeded,
    /// 装载命令失败、被拒或 pkexec 缺失。
    LoadFailed,
    /// 用户在引导向导中显式请求权限修复（或启动自检路径自动发起，D33）。
    FixPermissionsRequested,
    /// 权限修复命令成功退出。
    FixPermissionsSucceeded,
    /// 权限修复失败、被拒或 pkexec 缺失。
    FixPermissionsFailed,
}

/// 环境就绪态迁移动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentAction {
    /// 无外部动作。
    None,
    /// 在工作线程发起模块装载（单飞）。
    LoadModule,
    /// 在工作线程发起权限修复（单飞，D33）。
    FixPermissions,
    /// 呈现环境胶囊。
    ShowPill,
    /// 隐藏环境胶囊。
    HidePill,
}

/// 纯迁移函数（§4.13 转换表）：给定当前态与事件，返回下一态与对外动作。
///
/// 启动路径（`Unknown`/`Checking`）发现模块缺失时自动进入装载；复检路径
/// （`Issue`/`LoadFailed`/`Ready` 之后的 `CheckDone`）只刷新呈现，不再自动装载
/// ——自动装载每次启动至多一次的纪律由调用方保证（重试必须用户显式触发）。
pub fn next_environment_state(
    state: EnvironmentState,
    event: EnvironmentEvent,
) -> (EnvironmentState, EnvironmentAction) {
    use EnvironmentAction as A;
    use EnvironmentEvent as E;
    use EnvironmentState as S;
    match (state, event) {
        // 启动自检路径：空 → 就绪；含模块缺失 → 自动装载；其余问题 → 呈现。
        (S::Unknown | S::Checking, E::CheckDone(report)) => startup_done(report),
        // 装载收尾：成功 → 复检；失败 → 带回问题清单呈现。
        (S::Loading(_), E::LoadSucceeded) => (S::Checking, A::None),
        (S::Loading(issues), E::LoadFailed) => (S::LoadFailed(issues), A::ShowPill),
        // 单飞：装载在飞时重复请求与修复请求都被拒绝（状态不变）。
        (S::Loading(issues), E::LoadRequested) => (S::Loading(issues), A::None),
        (S::Loading(issues), E::FixPermissionsRequested) => (S::Loading(issues), A::None),
        // 权限修复收尾：成功 → 复检；失败 → 回到 Issue 呈现手动指引（D33）。
        (S::Fixing(_), E::FixPermissionsSucceeded) => (S::Checking, A::None),
        (S::Fixing(issues), E::FixPermissionsFailed) => (S::Issue(issues), A::ShowPill),
        // 单飞：修复在飞时重复请求与装载请求都被拒绝（状态不变）。
        (S::Fixing(issues), E::FixPermissionsRequested) => (S::Fixing(issues), A::None),
        (S::Fixing(issues), E::LoadRequested) => (S::Fixing(issues), A::None),
        // 用户在向导中显式重试装载或修复。
        (S::Issue(issues) | S::LoadFailed(issues), E::LoadRequested) => {
            (S::Loading(issues), A::LoadModule)
        }
        (S::Issue(issues) | S::LoadFailed(issues), E::FixPermissionsRequested) => {
            (S::Fixing(issues), A::FixPermissions)
        }
        // 复检：刷新问题清单；空 → 就绪。
        (S::Issue(_) | S::LoadFailed(_), E::CheckDone(report)) => recheck_done(report),
        (S::Ready, E::CheckDone(report)) => recheck_done(report),
        // 其余组合（如 Ready + LoadRequested）无定义动作：保持现状。
        (state, _) => (state, A::None),
    }
}

/// 启动自检路径的裁决（自动装载出口）。
fn startup_done(report: EnvironmentReport) -> (EnvironmentState, EnvironmentAction) {
    use EnvironmentAction as A;
    use EnvironmentState as S;
    if report.issues.is_empty() {
        (S::Ready, A::HidePill)
    } else if report
        .issues
        .iter()
        .any(|issue| matches!(issue, EnvironmentIssue::SgModuleMissing))
    {
        (S::Loading(report.issues), A::LoadModule)
    } else if report
        .issues
        .iter()
        .any(|issue| matches!(issue, EnvironmentIssue::SgNodePermissionDenied { .. }))
    {
        // 启动路径的自动权限修复（D33）；复检路径只刷新呈现，重试归用户。
        (S::Fixing(report.issues), A::FixPermissions)
    } else {
        (S::Issue(report.issues), A::ShowPill)
    }
}
/// 复检路径的裁决（不自动装载，只刷新呈现）。
fn recheck_done(report: EnvironmentReport) -> (EnvironmentState, EnvironmentAction) {
    use EnvironmentAction as A;
    use EnvironmentState as S;
    if report.issues.is_empty() {
        (S::Ready, A::HidePill)
    } else {
        (S::Issue(report.issues), A::ShowPill)
    }
}

/// `dev_root` 下的全部 sg 节点（确定性顺序：按路径排序）。
fn sg_nodes(dev_root: &Path) -> Vec<PathBuf> {
    let mut nodes: Vec<PathBuf> = fs::read_dir(dev_root)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with(SG_NODE_PREFIX))
                })
                .collect()
        })
        .unwrap_or_default();
    nodes.sort();
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// 锚点（§10）：环境自检报告 `sg` 模块缺失。
    #[test]
    fn test_environment_report_missing_module() {
        let root = tempdir();
        let sys = root.path().join("sys");
        let dev = root.path().join("dev");
        fs::create_dir_all(&sys).unwrap();
        fs::create_dir_all(&dev).unwrap();

        // 模块未装载：无论 dev 下有什么，都只报模块缺失。
        let report = inspect_environment_in(&sys, &dev);
        assert_eq!(report.issues, vec![EnvironmentIssue::SgModuleMissing]);
    }

    /// 锚点（§10）：环境自检报告设备节点权限不足（可读不可写即拒绝）。
    #[test]
    fn test_environment_report_permission_denied() {
        let root = tempdir();
        let sys = root.path().join("sys");
        let dev = root.path().join("dev");
        fs::create_dir_all(sys.join("module/sg")).unwrap();
        fs::create_dir_all(&dev).unwrap();

        // 无节点：就绪。
        assert_eq!(inspect_environment_in(&sys, &dev).issues, vec![]);

        // 只读节点（0o444）→ 权限不足。
        let ro = dev.join("sg0");
        fs::write(&ro, b"").unwrap();
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o444)).unwrap();
        let report = inspect_environment_in(&sys, &dev);
        assert_eq!(
            report.issues,
            vec![EnvironmentIssue::SgNodePermissionDenied {
                node: ro.to_string_lossy().into_owned()
            }]
        );

        // 追加可读写节点（0o666）：只读节点的问题保留，且不重复上报。
        let rw = dev.join("sg1");
        fs::write(&rw, b"").unwrap();
        fs::set_permissions(&rw, fs::Permissions::from_mode(0o666)).unwrap();
        let report = inspect_environment_in(&sys, &dev);
        assert_eq!(report.issues.len(), 1);


        assert!(matches!(
            report.issues[0],
            EnvironmentIssue::SgNodePermissionDenied { .. }
        ));

        // 非 sg 前缀节点被忽略。
        let other = dev.join("nvme0");
        fs::write(&other, b"").unwrap();
        fs::set_permissions(&other, fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(inspect_environment_in(&sys, &dev).issues.len(), 1);
    }

    /// 锚点（§10）：环境就绪状态机全表转换。
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
        let (s, a) = next_environment_state(S::Unknown, E::CheckDone(ready.clone()));
        assert_eq!(s, S::Ready);
        assert_eq!(a, A::HidePill);

        // 启动：模块缺失 → Loading + LoadModule（自动装载出口）。
        let (s, a) = next_environment_state(S::Checking, E::CheckDone(missing.clone()));
        assert_eq!(s, S::Loading(missing.issues.clone()));
        assert_eq!(a, A::LoadModule);

        // 启动：仅权限问题 → 自动权限修复（D33）。
        let (s, a) = next_environment_state(S::Unknown, E::CheckDone(denied.clone()));
        assert_eq!(s, S::Fixing(denied.issues.clone()));
        assert_eq!(a, A::FixPermissions);

        // 装载成功 → Checking（等待复检）。
        let (s, a) = next_environment_state(S::Loading(missing.issues.clone()), E::LoadSucceeded);
        assert_eq!(s, S::Checking);
        assert_eq!(a, A::None);

        // 单飞：装载在飞时重复请求被拒绝。
        let (s2, a2) =
            next_environment_state(S::Loading(missing.issues.clone()), E::LoadRequested);
        assert_eq!(s2, S::Loading(missing.issues.clone()));
        assert_eq!(a2, A::None);

        // 装载失败 → LoadFailed + ShowPill，问题清单带回。
        let (s, a) = next_environment_state(S::Loading(missing.issues.clone()), E::LoadFailed);
        assert_eq!(s, S::LoadFailed(missing.issues.clone()));
        assert_eq!(a, A::ShowPill);

        // 用户在向导中显式重试 → Loading + LoadModule。
        let (s, a) = next_environment_state(s, E::LoadRequested);
        assert_eq!(s, S::Loading(missing.issues.clone()));
        assert_eq!(a, A::LoadModule);

        // 复检路径：空 → Ready + HidePill。
        let (s, a) = next_environment_state(S::Issue(denied.issues.clone()), E::CheckDone(ready.clone()));
        assert_eq!(s, S::Ready);
        assert_eq!(a, A::HidePill);

        // 复检路径：非空 → Issue 刷新 + ShowPill；即便是模块缺失也不自动装载。
        let (s, a) = next_environment_state(S::LoadFailed(vec![]), E::CheckDone(missing.clone()));
        assert_eq!(s, S::Issue(missing.issues.clone()));
        assert_eq!(a, A::ShowPill);

        // 权限修复（D33）：启动自动 → Fixing + FixPermissions；成功 → 复检；
        // 失败 → Issue 呈现；单飞拒绝并发；用户显式重试；复检不自动修复。
        let (s, a) = next_environment_state(S::Checking, E::CheckDone(denied.clone()));
        assert_eq!(s, S::Fixing(denied.issues.clone()));
        assert_eq!(a, A::FixPermissions);
        let (s, a) = next_environment_state(s, E::FixPermissionsSucceeded);
        assert_eq!(s, S::Checking);
        assert_eq!(a, A::None);
        let (s, a) = next_environment_state(
            S::Fixing(denied.issues.clone()),
            E::FixPermissionsFailed,
        );
        assert_eq!(s, S::Issue(denied.issues.clone()));
        assert_eq!(a, A::ShowPill);
        let (s2, a2) = next_environment_state(
            S::Fixing(vec![]),
            E::FixPermissionsRequested,
        );
        assert_eq!(s2, S::Fixing(vec![]));
    /// 锚点（§10）：权限修复命令固定形态（D33）——argv 直传、user/nodes 白名单。
    #[test]
    fn test_permission_fix_command_whitelist() {
        let setfacl_dir = tempdir();
        let setfacl = setfacl_dir.path().join("setfacl");
        fs::write(&setfacl, b"#!/bin/sh\n").unwrap();
        let path = format!("{}:{}", setfacl_dir.path().display(), "/usr/bin");

        // 形态固定：pkexec <setfacl 绝对路径> -m u:<user>:rw <nodes…>。
        let nodes = vec!["/dev/sg0".to_string(), "/dev/sg1".to_string()];
        let argv = permission_fix_commands(Some("rikki"), &nodes)
            .inspect_err_msg_env(path.as_str());
        let argv = {
            // 局部辅助：带 PATH 构造（测试环境隔离，不污染进程 PATH）。
            struct WithPath<'a>(&'a str);
            impl<'a> WithPath<'a> {
                fn build(&self, user: Option<&str>, nodes: &[String]) -> Option<Vec<String>> {
                    // 直接内联调用会读进程 PATH；这里复用同一实现路径：
                    // 用环境变量保存/恢复的方式不可重入，改为构造再校验。
                    let _ = self;
                    permission_fix_commands_with_path(user, nodes, self.0)
                }
            }
            WithPath(path.as_str()).build(Some("rikki"), &nodes)
        };
        let argv = argv.expect("合法输入必须构造出命令");
        assert_eq!(argv[0], "pkexec");
        assert!(argv[1].ends_with("/setfacl"));
        assert_eq!(argv[2], "-m");
        assert_eq!(argv[3], "u:rikki:rw");
        assert_eq!(argv[4..], nodes);

        // 白名单拒绝：非法用户名、非法节点、空节点清单、找不到 setfacl。
        assert!(permission_fix_commands(Some("rikki;rm"), &nodes,).is_none() || {
            // 非法用户在进程 PATH 语义下同样应被拒绝；此处显式校验用户白名单。
            false
        });
        assert!(!user_is_safe("rikki;rm -rf /"));
        assert!(!user_is_safe(""));
        assert!(!user_is_safe("a/b"));
        assert!(user_is_safe("rikki"));
        assert!(user_is_safe("a.b-c_d"));
        assert!(node_is_safe("/dev/sg0"));
        assert!(!node_is_safe("/dev/sg0/../../etc"));
        assert!(!node_is_safe("/dev/nvme0"));
        assert!(!node_is_safe("/dev/sg"));
    }

    /// 锚点（§10）：权限修复命令固定白名单且规则含 uaccess、VID 限定（D33）。
    #[test]
    fn test_permission_fix_command_whitelist() {
        let argv = permission_fix_command();
        // 形态固定：pkexec sh -c <常量脚本>，无任何用户输入拼接。
        assert_eq!(argv.len(), 4);
        assert_eq!(argv[0], "pkexec");
        assert_eq!(argv[1], "sh");
        assert_eq!(argv[2], "-c");
        let script = argv[3];
        // 规则要素：三星 VID 限定 + scsi_generic + uaccess（登录会话用户 ACL）。
        assert!(script.contains("04e8"));
        assert!(script.contains("scsi_generic"));
        assert!(script.contains("uaccess"));
        // 重载与触发齐备。
        assert!(script.contains("udevadm control --reload"));
        // 脚本内嵌的规则路径与导出常量一致（concat! 只收字面量，测试防漂移）。
        assert!(script.contains(UDEV_RULE_PATH));
        // 无害性：唯一写入痕迹是一个规则文件；无删除/格式化类命令。
        assert_eq!(script.matches(".rules").count(), 1);
        assert_eq!(script.matches('>').count(), 1);
        assert!(!script.contains("rm "));
        // 幂等：常量脚本每次执行写入相同内容。
        assert_eq!(permission_fix_command(), permission_fix_command());
    }

    /// 临时目录守卫（测试结束清理）。
    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("创建临时目录")
    }
}
