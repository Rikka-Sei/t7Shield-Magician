//! §4.13 引导向导（D30；D29：界面由 Rust 代码构建，libadwaita 原生组件）。
//!
//! 逐项列出环境问题（`EnvironmentIssue` 一行：标题 + 建议动作文案），并提供两个修复
//! 动作：重新装载内核模块（固定命令 `pkexec modprobe sg`）与重新执行环境自检。
//! 环境状态机每次迁移后由主窗口调用 [`EnvironmentDialog::reload`] 刷新内容。

use std::cell::RefCell;

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use rust_i18n::t;

use crate::environment::EnvironmentIssue;
use crate::ui::MainWindow;

mod imp {
    use super::*;

    /// 引导向导子件（D29：全部在 Rust 侧构建）。
    pub struct EnvironmentDialog {
        pub toolbar: adw::ToolbarView,
        pub dialog_title: adw::WindowTitle,
        pub page: adw::PreferencesPage,
        pub issues_group: adw::PreferencesGroup,
        pub actions_group: adw::PreferencesGroup,
        pub action_load: adw::ButtonRow,
        pub action_fix: adw::ButtonRow,
        pub action_recheck: adw::ButtonRow,
        pub(crate) window: RefCell<Option<glib::WeakRef<MainWindow>>>,
        /// 当前呈现中的问题行（reload 时整组替换）。
        pub(crate) issue_rows: RefCell<Vec<adw::ActionRow>>,
        /// 最近一次刷新的问题清单（relocalize 重建行文案用）。
        pub(crate) last_issues: RefCell<Vec<EnvironmentIssue>>,
        /// 最近一次刷新的装载在飞标记。
        pub(crate) last_loading: std::cell::Cell<bool>,
    }

    impl Default for EnvironmentDialog {
        fn default() -> Self {
            let issues_group = adw::PreferencesGroup::new();
            let action_load = adw::ButtonRow::new();
            action_load.set_start_icon_name(Some("system-run-symbolic"));
            let action_fix = adw::ButtonRow::new();
            action_fix.set_start_icon_name(Some("emblem-system-symbolic"));
            let action_recheck = adw::ButtonRow::new();
            action_recheck.set_start_icon_name(Some("view-refresh-symbolic"));
            let actions_group = adw::PreferencesGroup::new();
            actions_group.add(&action_load);
            actions_group.add(&action_fix);
            actions_group.add(&action_recheck);
            let page = adw::PreferencesPage::new();
            page.add(&issues_group);
            page.add(&actions_group);
            // 标题栏：AdwHeaderBar 在 AdwDialog 内自动呈现关闭按钮（口令对话框同模式）。
            let dialog_title = adw::WindowTitle::new("", "");
            let header = adw::HeaderBar::new();
            header.set_title_widget(Some(&dialog_title));
            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&header);
            toolbar.set_content(Some(&page));
            Self {
                toolbar,
                dialog_title,
                page,
                issues_group,
                actions_group,
                action_load,
                action_fix,
                action_recheck,
                window: RefCell::new(None),
                issue_rows: RefCell::new(Vec::new()),
                last_issues: RefCell::new(Vec::new()),
                last_loading: std::cell::Cell::new(false),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EnvironmentDialog {
        const NAME: &'static str = "MagiEnvironmentDialog";
        type Type = super::EnvironmentDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for EnvironmentDialog {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            // 高度随内容自适应：固定高度会在问题清单短的时候留下大片空白。
            obj.set_content_width(480);
            obj.set_title(&t!("environment.wizard_title"));
            obj.set_child(Some(&self.toolbar));
        }
    }

    impl WidgetImpl for EnvironmentDialog {}
    impl adw::subclass::prelude::AdwDialogImpl for EnvironmentDialog {}
}

glib::wrapper! {
    /// 引导向导（D30）：环境问题清单 + 修复动作；可从环境胶囊再次打开。
    pub struct EnvironmentDialog(ObjectSubclass<imp::EnvironmentDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

impl EnvironmentDialog {
    /// 构造对话框：装配文案、当前问题清单与装载状态，并接线修复动作。
    pub fn new(window: &MainWindow, issues: &[EnvironmentIssue], loading: bool) -> Self {
        let dialog: Self = glib::Object::new();
        *dialog.imp().window.borrow_mut() = Some(window.downgrade());
        dialog.relocalize();
        dialog.reload(issues, loading);
        dialog.connect_rows();
        dialog
    }

    /// 刷新问题列表与动作行状态（环境状态机每次迁移后调用）。
    pub fn reload(&self, issues: &[EnvironmentIssue], loading: bool) {
        let imp = self.imp();
        *imp.last_issues.borrow_mut() = issues.to_vec();
        imp.last_loading.set(loading);
        // 重建问题行：先移除旧行，再按确定性顺序补新行。
        for row in imp.issue_rows.borrow().iter() {
            imp.issues_group.remove(row);
        }
        imp.issue_rows.borrow_mut().clear();
        if issues.is_empty() {
            let row = adw::ActionRow::new();
            row.set_title(&t!("environment.empty"));
            row.set_activatable(false);
            imp.issues_group.add(&row);
            imp.issue_rows.borrow_mut().push(row);
        } else {
            for issue in issues {
                let row = adw::ActionRow::new();
                match issue {
                    EnvironmentIssue::SgModuleMissing => {
                        row.set_title(&t!("environment.issue_module_title"));
                        row.set_subtitle(&t!("environment.issue_module_advice"));
                    }
                    EnvironmentIssue::SgNodePermissionDenied { node } => {
                        row.set_title(&t!(
                            "environment.issue_permission_title",
                            node = node.as_str()
                        ));
                        row.set_subtitle(&t!("environment.issue_permission_advice"));
                    }
                }
                row.set_activatable(false);
                imp.issues_group.add(&row);
                imp.issue_rows.borrow_mut().push(row);
            }
        }
        // 修复动作在飞：动作行转为忙碌呈现并禁用（单飞拒绝重入，状态机同样兜底）。
        let load_key = if loading {
            "environment.action_load_busy"
        } else {
            "environment.action_load"
        };
        let fix_key = if loading {
            "environment.action_fix_busy"
        } else {
            "environment.action_fix"
        };
        imp.action_load.set_title(&t!(load_key));
        imp.action_fix.set_title(&t!(fix_key));
        imp.action_load.set_sensitive(!loading);
        imp.action_fix.set_sensitive(!loading);
        imp.action_recheck.set_sensitive(!loading);
        // 修复动作按问题相关性呈现：装载行仅在模块缺失、修复行仅在权限问题。
        let module_missing = issues
            .iter()
            .any(|issue| matches!(issue, EnvironmentIssue::SgModuleMissing));
        let permission_denied = issues
            .iter()
            .any(|issue| matches!(issue, EnvironmentIssue::SgNodePermissionDenied { .. }));
        imp.action_load.set_visible(module_missing || loading);
        imp.action_fix.set_visible(permission_denied || loading);
    }

    /// 语言切换后重设对话框自身文案（问题行按缓存清单重建）。
    pub fn relocalize(&self) {
        let imp = self.imp();
        self.set_title(&t!("environment.wizard_title"));
        imp.dialog_title.set_title(&t!("environment.wizard_title"));
        imp.issues_group.set_title(&t!("environment.issues_group"));
        imp.action_recheck.set_title(&t!("environment.action_recheck"));
        let issues = imp.last_issues.borrow().clone();
        let loading = imp.last_loading.get();
        self.reload(&issues, loading);
    }

    /// 修复动作接线：转发到主窗口的环境状态机入口。
    fn connect_rows(&self) {
        let imp = self.imp();
        let this = self.clone();
        imp.action_load.connect_activated(glib::clone!(
            #[weak]
            this,
            move |_| {
                if let Some(window) = this
                    .imp()
                    .window
                    .borrow()
                    .as_ref()
                    .and_then(|weak| weak.upgrade())
                {
                    window.environment_load_requested();
                }
            }
        ));
        let this = self.clone();
        imp.action_fix.connect_activated(glib::clone!(
            #[weak]
            this,
            move |_| {
                if let Some(window) = this
                    .imp()
                    .window
                    .borrow()
                    .as_ref()
                    .and_then(|weak| weak.upgrade())
                {
                    window.environment_fix_requested();
                }
            }
        ));
        let this = self.clone();
        imp.action_recheck.connect_activated(glib::clone!(
            #[weak]
            this,
            move |_| {
                if let Some(window) = this
                    .imp()
                    .window
                    .borrow()
                    .as_ref()
                    .and_then(|weak| weak.upgrade())
                {
                    window.environment_recheck_requested();
                }
            }
        ));
    }
}
