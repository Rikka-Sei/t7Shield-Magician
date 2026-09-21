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

    /// 当前问题清单与装载在飞标记的呈现快照。
    pub(crate) fn environment_snapshot(&self) -> (Vec<environment::EnvironmentIssue>, bool) {
        let state = self.state().borrow();
        match &state.environment {
            EnvironmentState::Issue(issues) | EnvironmentState::LoadFailed(issues) => {
                (issues.clone(), false)
            }
            EnvironmentState::Loading(issues) | EnvironmentState::Fixing(issues) => {
                (issues.clone(), true)
            }
            _ => (Vec::new(), false),
        }
    }

    /// 打开引导向导（D30）：问题清单快照 + 装载状态；迁移时经弱引用刷新。
    pub(crate) fn present_environment_dialog(&self) {
        let (issues, loading) = self.environment_snapshot();
        let dialog = EnvironmentDialog::new(self, &issues, loading);
        self.state().borrow_mut().open_environment_dialog = Some(dialog.downgrade());
        dialog.present(Some(self));
    }

    /// 向导打开期间的状态迁移刷新（§4.13）。
    pub(crate) fn refresh_environment_dialog(&self) {
        let dialog = self
            .state()
            .borrow()
            .open_environment_dialog
            .as_ref()
            .and_then(|weak| weak.upgrade());
        if let Some(dialog) = dialog {
            let (issues, loading) = self.environment_snapshot();
            dialog.reload(&issues, loading);
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
