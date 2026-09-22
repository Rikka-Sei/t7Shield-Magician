//! 环境编排（D30/D33）：环境状态机宿主——事件入口、装载/修复工作线程、
//! 胶囊显隐与引导向导接线。界面元素在 main_window 构建，本文件只做编排（D29 分文件）。

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use crate::device::environment::{self, EnvironmentEvent, EnvironmentState, next_environment_state};
use crate::ui::MainWindow;
use crate::ui::environment_dialog::EnvironmentDialog;

/// 环境域在飞操作键（向导各行忙碌文案与可见性按此键控，不共用单一布尔）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InFlight {
    Loading,
    Fixing,
}

impl MainWindow {
    /// 环境状态机入口（§4.13）：纯迁移 + 动作落位（一次性纪律由状态机与 env_autos 保证）。
    pub(crate) fn apply_environment_event(&self, event: EnvironmentEvent) {
        eprintln!("[env] event={:?} state={:?}", event, self.imp().env.borrow().state);
        let action = {
            let mut env = self.imp().env.borrow_mut();
            let (next, action) =
                next_environment_state(env.state.clone(), event, &mut env.autos);
            env.state = next;
            action
        };
        match action {
            environment::EnvironmentAction::None => {}
            environment::EnvironmentAction::ShowPill => {
                self.imp().header.environment_pill.set_visible(true)
            }
            environment::EnvironmentAction::HidePill => {
                self.imp().header.environment_pill.set_visible(false)
            }
            environment::EnvironmentAction::LoadModule => self.spawn_module_load(),
            environment::EnvironmentAction::FixPermissions => self.spawn_permission_fix(),
        }
        eprintln!("[env] action={:?} new_state={:?}", action, self.imp().env.borrow().state);
        self.refresh_environment_dialog();
    }

    /// 模块装载（§4.13）：固定命令 `pkexec modprobe sg` 在工作线程执行（单飞由
    /// 状态机保证：Loading 态拒绝并发 LoadRequested）；结果回主线程复检一轮。
    pub(crate) fn spawn_module_load(&self) {
        let (sender, receiver) = async_channel::unbounded::<bool>();
        std::thread::spawn(move || {
            let argv = environment::module_load_command();
            let ok = std::process::Command::new(argv[0])
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
                    EnvironmentEvent::LoadSucceeded
                } else {
                    EnvironmentEvent::LoadFailed
                });
                // 收尾后统一复检：成功 → Ready；失败 → 问题清单刷新（仍是 Issue/LoadFailed）。
                let report = environment::inspect_environment();
                this.apply_environment_event(EnvironmentEvent::CheckDone(report));
            }
        });
    }

    /// 权限修复（§4.13，D33）：argv 直传 pkexec setfacl；前置不满足不发起、直接手动指引。
    pub(crate) fn spawn_permission_fix(&self) {
        let nodes: Vec<String> = {
            let env = self.imp().env.borrow();
            match &env.state {
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
        let user = environment::current_user();
        eprintln!(
            "[env-fix] user={:?} nodes={:?} path_has_setfacl={:?}",
            user,
            nodes,
            std::env::var("PATH").ok().map(|p| p.split(':').any(|d| std::path::Path::new(d).join("setfacl").is_file()))
        );
        let argv = environment::permission_fix_commands(user.as_deref(), &nodes);
        eprintln!("[env-fix] argv={:?}", argv);
        let Some(argv) = argv else {
            eprintln!("[env-fix] 前置不满足，走 FixPermissionsFailed");
            self.apply_environment_event(EnvironmentEvent::FixPermissionsFailed);
            return;
        };
        let (sender, receiver) = async_channel::unbounded::<bool>();
        std::thread::spawn(move || {
            let ok = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .status()
                .map(|status| {
                    eprintln!("[env-fix] pkexec exit={:?}", status.code());
                    status.success()
                })
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

    /// 当前问题清单与在飞操作键的呈现快照（Loading→装载在飞、Fixing→修复在飞）。
    pub(crate) fn environment_snapshot(
        &self,
    ) -> (Vec<environment::EnvironmentIssue>, Option<InFlight>) {
        let env = self.imp().env.borrow();
        match &env.state {
            EnvironmentState::Issue(issues) | EnvironmentState::LoadFailed(issues) => {
                (issues.clone(), None)
            }
            EnvironmentState::Loading(issues) => (issues.clone(), Some(InFlight::Loading)),
            EnvironmentState::Fixing(issues) => (issues.clone(), Some(InFlight::Fixing)),
            _ => (Vec::new(), None),
        }
    }


    /// 打开引导向导（D30）：问题清单快照 + 装载状态；迁移时经弱引用刷新。
    pub(crate) fn present_environment_dialog(&self) {
        let (issues, in_flight) = self.environment_snapshot();
        let dialog = EnvironmentDialog::new(self, &issues, in_flight);
        self.imp().dialogs.borrow_mut().environment = Some(dialog.downgrade());
        dialog.present(Some(self));
    }

    /// 向导打开期间的状态迁移刷新（§4.13）。
    pub(crate) fn refresh_environment_dialog(&self) {
        let dialog = self
            .imp()
            .dialogs
            .borrow()
            .environment
            .as_ref()
            .and_then(|weak| weak.upgrade());
        if let Some(dialog) = dialog {
            let (issues, in_flight) = self.environment_snapshot();
            dialog.reload(&issues, in_flight);
        }
    }

    /// 引导向导「装载内核模块」入口（§4.13：用户显式触发，单飞由状态机兜底）。
    pub fn environment_load_requested(&self) {
        self.apply_environment_event(EnvironmentEvent::LoadRequested);
    }

    /// 引导向导「修复设备权限」入口（§4.13，D33：用户显式触发，单飞由状态机兜底）。
    pub fn environment_fix_requested(&self) {
        self.apply_environment_event(EnvironmentEvent::FixPermissionsRequested);
    }

    /// 引导向导「重新检查」入口（§4.13）。
    pub fn environment_recheck_requested(&self) {
        let report = environment::inspect_environment();
        self.apply_environment_event(EnvironmentEvent::CheckDone(report));
    }
}
