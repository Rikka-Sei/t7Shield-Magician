//! §4.12 主窗口：设备卡片、锁定状态、操作入口、进度与结果反馈。
//!
//! 结构全部来自 `ui/main_window.ui`（K5：`.ui` 只放 id/class，文案零字面量），本文件只做
//! 文案赋值、入口敏感度刷新与流程装配；协议 I/O 一律经 [`crate::jobs`] 在工作线程执行。
//! 布局全部使用 libadwaita/GTK 原生组件与内置样式类（D28：零自定义 CSS）；结果区经
//! [`Outcome`] 语义分级（success/error 内置类 + symbolic 图标）统一呈现。
//!
//! 主流程（§4.13）：启动即扫描设备 → 更新设备卡片/状态/入口 → 用户触发 → 口令对话框 →
//! 工作线程作业 → `AppEvent` 回主线程更新进度与结果；取消只停止后续步骤，不阻塞退出（§6）。
//! 设备热插拔（§4.1 REQ-001）：工作线程周期重扫，经 MainContext channel 回主线程，
//! 由重扫状态机（[`crate::controller::RescanState`]）统一裁决设备卡片与入口更新。

use std::cell::RefCell;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use gtk::subclass::prelude::*;
use gtk::CompositeTemplate;
use gtk4 as gtk;
use libadwaita as adw;
use magi_protocol::{Password, UnlockEvidence, UnlockStep};
use rust_i18n::t;

use crate::controller::{ActionId, DeviceIdentity, RescanAction, RescanState, UnlockGate};
use crate::diagnostics::{self, Level};
use crate::jobs::{self, CancelFlag, DeviceJob, ScanHit};
use crate::password_dialog::PasswordDialog;
use crate::presentation::{self, AppError, AppEvent};

/// 侧边栏导航项（对标原版 Magician 的分组导航列表；选中态恒唯一）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavItem {
    /// 仪表盘：设备卡片、操作入口、进度与结果。
    Dashboard,
    /// 诊断：环形缓冲记录与脱敏导出。
    Diagnostics,
    /// 关于：版本与协议引用。
    About,
}

impl NavItem {
    /// 导航项全集（顺序即呈现顺序）。
    pub const ALL: [NavItem; 3] = [NavItem::Dashboard, NavItem::Diagnostics, NavItem::About];

    /// 页面栈中的页名。
    pub fn page_name(self) -> &'static str {
        match self {
            NavItem::Dashboard => "dashboard",
            NavItem::Diagnostics => "diagnostics",
            NavItem::About => "about",
        }
    }

    /// 导航项标签的 i18n 键。
    pub fn label_key(self) -> &'static str {
        match self {
            NavItem::Dashboard => "nav.dashboard",
            NavItem::Diagnostics => "nav.diagnostics",
            NavItem::About => "nav.about",
        }
    }

    /// 选中态：给定当前页，返回每个导航项的 `(项, 是否选中)`；选中项恰 1 个。
    pub fn selection_states(active: NavItem) -> Vec<(NavItem, bool)> {
        NavItem::ALL
            .into_iter()
            .map(|item| (item, item == active))
            .collect()
    }
}

/// 锁定状态徽章的语义类（libadwaita/GTK 内置，不依赖自定义样式表）：锁定 `warning`、
/// 解锁 `success`、其余（重枚举/未识别/无设备）`dim-label`。
pub fn badge_class(identity: DeviceIdentity) -> &'static str {
    match identity {
        DeviceIdentity::Locked => "warning",
        DeviceIdentity::Unlocked => "success",
        DeviceIdentity::ReEnumerating | DeviceIdentity::Unrecognized => "dim-label",
    }
}
/// 结果区语义分级（内置类）：中性（默认文本，无图标）、成功（success + emblem-ok）、
/// 错误（error + dialog-warning）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Neutral,
    Success,
    Error,
}

/// 结果键 → 语义分级：判据确认/口令被接受为成功；其余（含取消、未观察到重枚举）为中性。
pub(crate) fn result_kind(key: &str) -> Outcome {
    match key {
        "result.RealPartitionTable" | "result.LockingFlags" | "result.validate_accepted" => {
            Outcome::Success
        }
        "diagnostics.export_failed" => Outcome::Error,
        _ => Outcome::Neutral,
    }
}

/// 徽章图标（语义与徽章类一一对应，全部为 Adwaita 图标主题自带 symbolic 图标）。
pub(crate) fn status_icon_name(identity: DeviceIdentity) -> &'static str {
    match identity {
        DeviceIdentity::Locked => "changes-prevent-symbolic",
        DeviceIdentity::Unlocked => "emblem-ok-symbolic",
        DeviceIdentity::ReEnumerating => "view-refresh-symbolic",
        DeviceIdentity::Unrecognized => "dialog-question-symbolic",
    }
}

#[derive(Debug, Default)]
pub(crate) struct WindowState {
    gate: UnlockGate,
    device: Option<DeviceJob>,
    cancel: CancelFlag,
    action: Option<ActionId>,
    /// 周期重扫状态机：呈现中身份、在位缓存与空态防抖计数。
    rescan: RescanState,
}

mod imp {
    use super::*;

    #[derive(CompositeTemplate, Default)]
    #[template(file = "ui/main_window.ui")]
    pub struct MainWindow {
        #[template_child]
        pub device_card: TemplateChild<adw::Bin>,
        #[template_child]
        pub status_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub action_unlock: TemplateChild<gtk::Button>,
        #[template_child]
        pub progress: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub result_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub window_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub brand_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub nav_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub nav_dashboard: TemplateChild<gtk::ListBoxRow>,
        #[template_child]
        pub nav_diagnostics: TemplateChild<gtk::ListBoxRow>,
        #[template_child]
        pub nav_about: TemplateChild<gtk::ListBoxRow>,
        #[template_child]
        pub nav_dashboard_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub nav_diagnostics_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub nav_about_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub content_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub dashboard_title_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub diagnostics_title_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub diagnostics_hint_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub diagnostics_view: TemplateChild<gtk::TextView>,
        #[template_child]
        pub about_title_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub about_version_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub about_repository_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub about_notice_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub sidebar_notice_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub device_group_title: TemplateChild<gtk::Label>,
        #[template_child]
        pub device_model_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub device_ids_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub device_node_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub device_channel_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub device_descriptor_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub action_validate_password: TemplateChild<gtk::Button>,
        #[template_child]
        pub action_set_password: TemplateChild<gtk::Button>,
        #[template_child]
        pub action_change_password: TemplateChild<gtk::Button>,
        #[template_child]
        pub action_delete_password: TemplateChild<gtk::Button>,
        #[template_child]
        pub action_cancel: TemplateChild<gtk::Button>,
        #[template_child]
        pub action_export_diagnostics: TemplateChild<gtk::Button>,
        #[template_child]
        pub split_view: TemplateChild<adw::OverlaySplitView>,
        #[template_child]
        pub sidebar_toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub device_area_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub empty_state: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub status_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub result_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub progress_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub password_admin_note: TemplateChild<gtk::Label>,
        #[template_child]
        pub actions_caption: TemplateChild<gtk::Label>,
        #[template_child]
        pub feedback_caption: TemplateChild<gtk::Label>,
        #[template_child]
        pub device_node_caption: TemplateChild<gtk::Label>,
        #[template_child]
        pub device_channel_caption: TemplateChild<gtk::Label>,
        #[template_child]
        pub device_descriptor_caption: TemplateChild<gtk::Label>,
        #[template_child]
        pub platform_notice_label: TemplateChild<gtk::Label>,
        pub(crate) state: RefCell<WindowState>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MainWindow {
        const NAME: &'static str = "MagiMainWindow";
        type Type = super::MainWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            // 实例化模板：模板子件在此绑定（未绑定会在 `check_template_children` 处报错）。
            obj.init_template();
        }
    }

    impl ObjectImpl for MainWindow {}
    impl WidgetImpl for MainWindow {}
    impl gtk::subclass::prelude::WindowImpl for MainWindow {}
    impl gtk::subclass::prelude::ApplicationWindowImpl for MainWindow {}
    impl adw::subclass::prelude::AdwApplicationWindowImpl for MainWindow {}
}

glib::wrapper! {
    /// 主窗口（§4.12）。
    pub struct MainWindow(ObjectSubclass<imp::MainWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Native,
                    gtk::Root, gtk::ShortcutManager, gio::ActionGroup, gio::ActionMap;
}

impl Default for MainWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl MainWindow {
    /// 构造主窗口：装载模板、装配文案与初始入口状态。
    pub fn new() -> Self {
        let window: Self = glib::Object::new();
        window.setup();
        window
    }

    /// 把主窗口挂到应用上：激活时呈现窗口、接线入口、订阅事件并扫描设备。
    pub fn install(app: &adw::Application) {
        app.connect_activate(|app| {
            let window = MainWindow::new();
            window.set_application(Some(app));
            window.connect_close_request(glib::clone!(
                #[weak]
                window,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |_| {
                    // §6：关闭窗口 → 当前命令返回后停止后续步骤；不阻塞退出。
                    window.cancel_current();
                    glib::Propagation::Proceed
                }
            ));
            window.present();
            window.start();
        });
    }

    /// 启动装配（§4.13）：注册事件汇、接线入口、扫描设备并启动周期重扫监控。
    pub fn start(&self) {
        let this = self.clone();
        jobs::set_event_sink(move |event| this.on_event(event));
        self.connect_actions();
        self.refresh_devices();
        self.refresh_actions();
        self.spawn_device_watch();
    }

    /// 文案与初始状态：`.ui` 内无字面量，全部显示文本在此经 i18n 键赋值。
    fn setup(&self) {
        let imp = self.imp();
        imp.window_title.set_title(&t!("app.title"));
        imp.window_title.set_subtitle(&t!("app.subtitle"));
        imp.brand_label.set_label(&t!("app.title"));
        imp.nav_dashboard_label
            .set_label(&t!(NavItem::Dashboard.label_key()));
        imp.nav_diagnostics_label
            .set_label(&t!(NavItem::Diagnostics.label_key()));
        imp.nav_about_label
            .set_label(&t!(NavItem::About.label_key()));
        imp.content_stack
            .set_visible_child_name(NavItem::Dashboard.page_name());
        // 初始选中第一项:选中态由 GtkListBox 原生机制维护(此时导航回调尚未接线,
        // 不会重入 show_page;页面可见性已由上一行直接设置)。
        if let Some(row) = imp.nav_list.row_at_index(0) {
            imp.nav_list.select_row(Some(&row));
        }
        imp.dashboard_title_label.set_label(&t!("dashboard.title"));
        imp.diagnostics_title_label
            .set_label(&t!("diagnostics.view_title"));
        imp.diagnostics_hint_label
            .set_label(&t!("diagnostics.hint"));
        imp.about_title_label.set_label(&t!("nav.about"));
        imp.about_version_label
            .set_label(&t!("about.version", version = env!("CARGO_PKG_VERSION")));
        imp.about_repository_label
            .set_label(&t!("about.repository"));
        imp.about_notice_label.set_label(&t!("about.notice"));
        imp.sidebar_notice_label.set_label(&t!("app.subtitle"));
        imp.device_group_title.set_label(&t!("device.group_title"));
        imp.device_model_label.set_label(&t!("device.model"));
        imp.device_node_caption.set_label(&t!("device.node"));
        imp.device_channel_caption.set_label(&t!("device.channel_title"));
        imp.device_descriptor_caption
            .set_label(&t!("device.descriptor_title"));
        imp.actions_caption.set_label(&t!("dashboard.actions_title"));
        imp.feedback_caption.set_label(&t!("dashboard.feedback_title"));
        imp.password_admin_note.set_label(&t!("reason.evidence_gap"));
        imp.empty_state
            .set_title(&t!(DeviceIdentity::Unrecognized.status_key()));
        imp.empty_state
            .set_description(Some(&t!("device.empty_hint")));
        imp.sidebar_toggle
            .set_tooltip_text(Some(&t!("nav.toggle_sidebar")));
        // 窄窗口折叠时才显示侧边栏开关；开关与 show-sidebar 双向同步（原生绑定）。
        imp.split_view
            .bind_property("collapsed", &imp.sidebar_toggle.get(), "visible")
            .sync_create()
            .build();
        imp.sidebar_toggle
            .bind_property("active", &imp.split_view.get(), "show-sidebar")
            .bidirectional()
            .sync_create()
            .build();
        for action in ActionId::ALL {
            if let Some(button) = self.action_button(action) {
                button.set_label(&t!(action.label_key()));
            }
        }
        imp.action_cancel.set_label(&t!("action.cancel"));
        imp.action_export_diagnostics
            .set_label(&t!("action.export_diagnostics"));
        self.show_outcome(&t!("progress.idle"), Outcome::Neutral);
        self.show_unknown_device();
        self.apply_actions(DeviceIdentity::Unrecognized, &UnlockGate::new());
    }

    /// 入口接线（§4.12/§4.11/§6）。
    fn connect_actions(&self) {
        let imp = self.imp();
        for action in ActionId::ALL {
            let Some(button) = self.action_button(action) else {
                continue;
            };
            let this = self.clone();
            button.connect_clicked(glib::clone!(
                #[weak]
                this,
                move |_| this.trigger(action)
            ));
        }
        let this = self.clone();
        imp.action_cancel.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.cancel_current()
        ));
        let this = self.clone();
        imp.action_export_diagnostics.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.export_diagnostics()
        ));
        let this = self.clone();
        imp.nav_list.connect_row_selected(glib::clone!(
            #[weak]
            this,
            move |_, row| {
                // 选中行序与 `NavItem::ALL` 一致;点击行时 GTK 自动选中 → 此处切页。
                let Some(row) = row else {
                    return;
                };
                if let Some(item) = NavItem::ALL.get(row.index() as usize) {
                    this.show_page(*item);
                }
            }
        ));
    }

    /// 权威轮次的设备刷新（§4.1：Linux sysfs 扫描）。
    ///
    /// 同步取数并强制呈现（启动与会话收尾 §3.3：会话收尾后必须重新读取设备态再裁决
    /// 呈现）；扫描失败在此呈现为错误（§4.12）。周期轮次改走 [`Self::spawn_device_watch`]
    /// 的事件路径，失败只记诊断、不打扰结果区。
    fn refresh_devices(&self) {
        self.show_platform_notice();
        let outcome = jobs::fetch_scan();
        if let Some(error) = &outcome.error {
            self.show_error(error);
        }
        self.apply_scan(&outcome, true);
    }

    /// 呈现段（主线程）：按重扫状态机裁决并更新设备卡片、徽章与入口。
    ///
    /// `authoritative`：启动与会话收尾的轮次无视在飞状态强制收敛；周期轮次在作业在飞时
    /// 只更新在位缓存，不改呈现（§6：不打断用户正在看的进度与结果）。
    fn apply_scan(&self, outcome: &jobs::ScanOutcome, authoritative: bool) {
        let in_flight = !authoritative && jobs::any_job_in_flight();
        let identity = outcome.hits.first().map(|hit| hit.job.identity);
        let action = {
            let mut state = self.state().borrow_mut();
            let (next, action) = state.rescan.observe(identity, in_flight);
            state.rescan = next;
            action
        };
        match action {
            RescanAction::Keep => {}
            RescanAction::ShowHit => {
                let Some(hit) = outcome.hits.first() else {
                    return;
                };
                self.state().borrow_mut().device = Some(hit.job.clone());
                self.show_hit(hit);
                self.refresh_actions();
            }
            RescanAction::ShowEmpty => {
                self.state().borrow_mut().device = None;
                self.show_unknown_device();
                self.refresh_actions();
            }
        }
    }

    /// 设备热插拔监控装配（§4.1 REQ-001）：工作线程周期重扫，结果经 `async_channel`
    /// 回主线程 `MainContext` 消费（与作业事件同一通道形态）；GTK 侧只做装配。
    fn spawn_device_watch(&self) {
        let (sender, receiver) = async_channel::unbounded::<jobs::ScanOutcome>();
        if let Err(error) = jobs::spawn_scan_watch(sender) {
            // 监控线程创建失败与扫描失败同口径呈现（§5）；启动扫描仍已完成。
            self.show_error(&error);
            return;
        }
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Ok(outcome) = receiver.recv().await {
                if let Some(error) = &outcome.error {
                    // 周期轮次的扫描失败不进结果区（结果区属于作业呈现），只记诊断。
                    diagnostics::ring().record(Level::Warn, presentation::presentation_code(error));
                }
                this.apply_scan(&outcome, false);
            }
        });
    }

    /// 按设备态 + 重试预算刷新入口敏感度与禁用理由（§4.12/§4.11）。
    pub fn apply_actions(&self, identity: DeviceIdentity, gate: &UnlockGate) {
        for action in ActionId::ALL {
            let Some(button) = self.action_button(action) else {
                continue;
            };
            let enabled = gate.allows(action, identity.state());
            let reason_key = gate.disabled_reason_key(action, identity.state());
            button.set_sensitive(enabled);
            button.set_tooltip_text(reason_key.map(|key| t!(key).to_string()).as_deref());
        }
    }

    /// 按当前状态刷新入口（设备态与重试预算的两个来源在此汇合）。
    fn refresh_actions(&self) {
        let (identity, gate) = {
            let state = self.state().borrow();
            (
                state
                    .device
                    .as_ref()
                    .map(|job| job.identity)
                    .unwrap_or(DeviceIdentity::Unrecognized),
                state.gate,
            )
        };
        self.apply_actions(identity, &gate);
    }

    /// 设备卡片（§4.12：型号、VID:PID、设备节点、锁定状态、描述符摘要）。
    fn show_hit(&self, hit: &ScanHit) {
        let imp = self.imp();
        imp.device_area_stack.set_visible_child_name("card");
        imp.status_label
            .set_label(&t!(hit.job.identity.status_key()));
        self.set_badge_class(hit.job.identity);
        imp.device_model_label.set_label(&t!("device.model"));
        imp.device_ids_label.set_label(&format!(
            "{} {:04x}:{:04x}",
            t!("device.vid_pid"),
            hit.job.vid,
            hit.job.pid
        ));
        // 节点值单独成行，前缀说明由 `device_node_caption` 承担（§4.12）。
        imp.device_node_label.set_label(&match &hit.job.node {
            Some(node) => node.to_string(),
            None => String::new(),
        });
        imp.device_channel_label.set_label(&t!("device.channel"));
        imp.device_descriptor_label
            .set_label(hit.descriptor.as_deref().unwrap_or(""));
    }

    /// 设备卡片空态（§4.1：不识别为 T7 Shield，入口保持禁用）。
    fn show_unknown_device(&self) {
        let imp = self.imp();
        imp.device_area_stack.set_visible_child_name("empty");
        imp.status_label
            .set_label(&t!(DeviceIdentity::Unrecognized.status_key()));
        self.set_badge_class(DeviceIdentity::Unrecognized);
        imp.device_model_label.set_label(&t!("device.model"));
        imp.device_ids_label.set_label("");
        imp.device_node_label.set_label("");
        imp.device_channel_label.set_label("");
        imp.device_descriptor_label.set_label("");
    }

    /// 状态徽章配色（锁定态醒目暖色、解锁态绿色、其余中性）。
    fn set_badge_class(&self, identity: DeviceIdentity) {
        let imp = self.imp();
        for class in ["warning", "success", "dim-label"] {
            imp.status_label.remove_css_class(class);
            imp.status_icon.remove_css_class(class);
        }
        let class = badge_class(identity);
        imp.status_label.add_css_class(class);
        imp.status_icon.add_css_class(class);
        imp.status_icon.set_icon_name(Some(status_icon_name(identity)));
    }
    /// 平台说明文案（D27：Linux 通道说明）。
    pub fn show_platform_notice(&self) {
        let text = t!("platform.linux_notice").to_string();
        self.imp().platform_notice_label.set_label(&text);
    }

    /// 进度条与步骤文案（§4.13：7 个 `UnlockStep`）。
    pub fn show_step(&self, step: UnlockStep) {
        let total = UnlockStep::ALL.len() as f64;
        let done = UnlockStep::ALL
            .iter()
            .position(|candidate| *candidate == step)
            .map(|index| (index + 1) as f64)
            .unwrap_or(0.0);
        self.imp().progress_box.set_visible(true);
        self.imp().progress.set_fraction(done / total);
        self.imp()
            .result_label
            .set_label(&t!(format!("progress.{step}")));
    }

    /// 结果区统一呈现（§4.12/§6）：文本 + 语义类 + 图标；进度条随结果/错误隐藏。
    pub(crate) fn show_outcome(&self, text: &str, kind: Outcome) {
        let imp = self.imp();
        for class in ["success", "error", "dim-label"] {
            imp.result_label.remove_css_class(class);
            imp.result_icon.remove_css_class(class);
        }
        imp.result_label.set_label(text);
        match kind {
            Outcome::Neutral => imp.result_icon.set_visible(false),
            Outcome::Success => {
                imp.result_label.add_css_class("success");
                imp.result_icon.add_css_class("success");
                imp.result_icon.set_icon_name(Some("emblem-ok-symbolic"));
                imp.result_icon.set_visible(true);
            }
            Outcome::Error => {
                imp.result_label.add_css_class("error");
                imp.result_icon.add_css_class("error");
                imp.result_icon.set_icon_name(Some("dialog-warning-symbolic"));
                imp.result_icon.set_visible(true);
            }
        }
        imp.progress.set_fraction(0.0);
        imp.progress_box.set_visible(false);
    }

    /// 结果文案（§4.8 判据分级 / 取消 / 校验结论）。
    pub fn show_result(&self, message_key: &'static str) {
        self.show_outcome(&t!(message_key), result_kind(message_key));
    }

    /// 错误呈现：呈现码 + 一句原因 + 一句建议（§4.13）。
    pub fn show_error(&self, error: &AppError) -> String {
        let code = presentation::presentation_code(error);
        let text = format!(
            "{code}：{}；{}",
            t!(presentation::reason_key(error)),
            t!(presentation::advice_key(error)),
        );
        self.show_outcome(&text, Outcome::Error);
        diagnostics::ring().record(Level::Error, code);
        text
    }

    /// 窗口运行期状态（设备、闸门、取消标志与在飞动作）。
    fn state(&self) -> &RefCell<WindowState> {
        &self.imp().state
    }

    /// 切换页面并同步导航选中态(选中唯一性由 `GtkListBox` 原生选中机制保证)。
    fn show_page(&self, item: NavItem) {
        let imp = self.imp();
        imp.content_stack.set_visible_child_name(item.page_name());
        let index = NavItem::ALL
            .iter()
            .position(|candidate| *candidate == item)
            .expect("NavItem 必须在 ALL 中") as i32;
        if let Some(row) = imp.nav_list.row_at_index(index) {
            if !row.is_selected() {
                imp.nav_list.select_row(Some(&row));
            }
        }
        if item == NavItem::Diagnostics {
            self.refresh_diagnostics_view();
        }
    }

    /// 诊断页内容：环形缓冲的脱敏导出文本（只读、不可编辑）。
    fn refresh_diagnostics_view(&self) {
        let text = diagnostics::ring().export_redacted();
        let buffer = self.imp().diagnostics_view.buffer();
        buffer.set_text(&text);
    }

    /// 入口按钮（按 `ActionId` 取模板子件）。
    pub fn action_button(&self, action: ActionId) -> Option<gtk::Button> {
        let imp = self.imp();
        Some(match action {
            ActionId::Unlock => imp.action_unlock.get(),
            ActionId::ValidatePassword => imp.action_validate_password.get(),
            ActionId::SetPassword => imp.action_set_password.get(),
            ActionId::ChangePassword => imp.action_change_password.get(),
            ActionId::DeletePassword => imp.action_delete_password.get(),
        })
    }

    /// 入口触发（§4.12/§4.11）。
    fn trigger(&self, action: ActionId) {
        match action {
            ActionId::Unlock | ActionId::ValidatePassword => self.prompt_password(action),
            // §4.11：写口令入口受证据缺口约束；程序化触发也必须立即返回、不下发任何命令。
            ActionId::SetPassword | ActionId::ChangePassword => {
                self.show_error(&AppError::from(
                    magi_protocol::set_password(&[]).expect_err("口令写操作不可执行"),
                ));
            }
            ActionId::DeletePassword => {
                self.show_error(&AppError::from(
                    magi_protocol::delete_password(&[]).expect_err("口令写操作不可执行"),
                ));
            }
        }
    }

    /// 口令对话框 → 提交后启动作业（§4.12：提交即清零、对话框关闭并清除输入）。
    fn prompt_password(&self, action: ActionId) {
        // §6：重新打开对话框 → 本会话的口令重试预算复位。
        self.state().borrow_mut().gate.open_dialog();
        let dialog = PasswordDialog::new(action);
        let this = self.clone();
        dialog.submit_button().connect_clicked(glib::clone!(
            #[weak]
            this,
            #[weak]
            dialog,
            move |_| {
                let password = match dialog.take_password() {
                    Ok(password) => password,
                    Err(error) => {
                        // §4.12：空口令就地提示、对话框保持打开、不构造任何报文。
                        dialog.show_error(&error);
                        return;
                    }
                };
                dialog.close();
                this.start_job(action, password);
            }
        ));
        dialog.cancel_button().connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| {
                dialog.close();
            }
        ));
        dialog.present(Some(self));
    }

    /// 启动一次设备作业（§4.13：工作线程执行、事件回主线程；同设备单飞）。
    fn start_job(&self, action: ActionId, password: Password) {
        let Some(job) = self.state().borrow().device.clone() else {
            self.show_result(DeviceIdentity::Unrecognized.status_key());
            return;
        };
        let cancel = CancelFlag::new();
        {
            let mut state = self.state().borrow_mut();
            state.cancel = cancel.clone();
            state.action = Some(action);
        }
        diagnostics::ring().record(Level::Info, action.label_key());
        let spawned = jobs::spawn_device_job(job.device.clone(), move |emit| match action {
            ActionId::Unlock => jobs::unlock_device(&job, password, &cancel, emit),
            _ => jobs::validate_password(&job, password, emit),
        });
        if let Err(error) = spawned {
            // §4.13：同设备已有在飞操作 → `Busy`（命令队列上限 1，不排队）。
            self.show_error(&error);
        }
        self.refresh_actions();
    }

    /// 取消当前作业（§6：只停止后续步骤，不阻塞退出）。
    fn cancel_current(&self) {
        let cancel = self.state().borrow().cancel.clone();
        cancel.cancel();
    }

    /// 事件消费（主线程）：进度 / 收尾 / 失败（§4.13）。
    fn on_event(&self, event: AppEvent) {
        match event {
            AppEvent::Progress { step } => self.show_step(step),
            AppEvent::Finished { evidence } => self.finish(evidence),
            AppEvent::Failed { error } => self.fail(error),
        }
    }

    /// 收尾：按解锁判据分级呈现；取消优先于判据（§6）。
    fn finish(&self, evidence: Option<UnlockEvidence>) {
        let (action, cancelled) = {
            let state = self.state().borrow();
            let action = state.action;
            let cancelled = state.cancel.cancelled();
            (action, cancelled)
        };
        if cancelled {
            diagnostics::ring().record(Level::Info, "result.cancelled");
            self.show_result("result.cancelled");
        } else if action == Some(ActionId::ValidatePassword) {
            self.show_result("result.validate_accepted");
        } else {
            let (key, mounted) = match &evidence {
                Some(UnlockEvidence::RealPartitionTable { mounted_volumes }) => {
                    ("result.RealPartitionTable", mounted_volumes.clone())
                }
                Some(UnlockEvidence::LockingFlags { .. }) => ("result.LockingFlags", Vec::new()),
                Some(UnlockEvidence::PidChange { .. }) => ("result.PidChange", Vec::new()),
                None => ("result.none", Vec::new()),
            };
            diagnostics::ring().record(Level::Info, key);
            if mounted.is_empty() {
                self.show_result(key);
            } else {
                let text = format!(
                    "{} {}",
                    t!(key),
                    t!("result.mounted", volumes = mounted.join(", "))
                );
                self.show_outcome(&text, Outcome::Success);
            }
        }
        // §3.3：会话收尾后必须重新读取设备态再裁决呈现。
        self.refresh_devices();
    }

    /// 失败：记录口令被拒次数（§6：最多 3 次）并按呈现码呈现。
    fn fail(&self, error: AppError) {
        if error == AppError::PasswordRejected {
            self.state().borrow_mut().gate.record_rejection();
        }
        self.show_error(&error);
        self.refresh_actions();
    }

    /// 导出脱敏诊断文本（§6：用户显式导出，导出前再次过滤）。
    fn export_diagnostics(&self) {
        let dialog = gtk::FileDialog::builder()
            .title(t!("diagnostics.title"))
            .build();
        let this = self.clone();
        dialog.save(
            Some(self),
            gio::Cancellable::NONE,
            glib::clone!(
                #[weak]
                this,
                move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let text = diagnostics::ring().export_redacted();
                    if file
                        .replace_contents(
                            text.as_bytes(),
                            None,
                            false,
                            gio::FileCreateFlags::NONE,
                            gio::Cancellable::NONE,
                        )
                        .is_err()
                    {
                        this.show_result("diagnostics.export_failed");
                    }
                }
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation;

    /// `.ui` 结构断言（无头、跨平台）：子件 id 齐备、无用户可见字面量、口令输入不回显。
    ///
    /// 轻量扫描器只处理本项目的模板形态：收集 `id="…"`，并检查显示类属性不出现非空取值。
    #[test]
    fn test_ui_templates_declare_required_children() {
        let main: &str = include_str!("ui/main_window.ui");
        let ids = collect_ids(main);
        for required in [
            "device_card",
            "status_label",
            "action_unlock",
            "progress",
            "result_label",
            "device_area_stack",
            "empty_state",
            "progress_box",
            "status_icon",
            "result_icon",
            "sidebar_toggle",
        ] {
            assert!(
                ids.contains(&required.to_string()),
                "主窗口缺子件 {required}：{ids:?}"
            );
        }
        assert_no_display_literals(main);

        let dialog: &str = include_str!("ui/password_dialog.ui");
        let ids = collect_ids(dialog);
        for required in ["entry", "submit"] {
            assert!(
                ids.contains(&required.to_string()),
                "对话框缺子件 {required}：{ids:?}"
            );
        }
        assert_no_display_literals(dialog);
        // §4.12：口令输入不回显——`GtkPasswordEntry` 无 `visibility` 属性（输入恒被遮蔽），
        // 这里断言模板进一步关闭了「显示明文」图标，即对话框中不存在任何回显通路（§6）。
        assert_eq!(property_value(dialog, "visibility"), None);
        assert_eq!(
            property_value(dialog, "show-peek-icon"),
            Some("false".to_string())
        );
    }

    /// 锚点（§10）：侧边栏导航 ≥3 项且选中态唯一。
    #[test]
    fn test_sidebar_navigation_items() {
        // 结构：模板声明三个导航项与三个页面栈页名。
        let xml = strip_comments(include_str!("ui/main_window.ui"));
        let ids = collect_ids(&xml);
        for (id, item) in [
            ("nav_dashboard", NavItem::Dashboard),
            ("nav_diagnostics", NavItem::Diagnostics),
            ("nav_about", NavItem::About),
        ] {
            assert!(
                ids.contains(&id.to_string()),
                "侧边栏缺导航项 {id}：{ids:?}"
            );
            assert!(
                xml.contains(&format!(
                    "<property name=\"name\">{}</property>",
                    item.page_name()
                )),
                "页面栈缺页 {}",
                item.page_name()
            );
        }
        assert!(NavItem::ALL.len() >= 3, "导航项至少 3 项");
        // 模板:导航容器挂内置 `.navigation-sidebar` 类,选中态由 GtkListBox 原生
        // 选中机制保证(同一时刻至多一行选中,不再使用自定义 active 类)。
        assert!(
            xml.contains("<class name=\"navigation-sidebar\"/>"),
            "侧边栏导航必须用内置 navigation-sidebar 类"
        );

        // 纯逻辑：任意当前页下选中态恰 1 项且落在该项；页名与标签键互不重复且都能取到文案。
        let mut pages = std::collections::BTreeSet::new();
        let mut keys = std::collections::BTreeSet::new();
        for item in NavItem::ALL {
            let states = NavItem::selection_states(item);
            assert_eq!(states.len(), NavItem::ALL.len());
            assert_eq!(
                states.iter().filter(|(_, active)| *active).count(),
                1,
                "{item:?} 的选中态必须唯一"
            );
            assert!(states.contains(&(item, true)));
            assert!(states
                .iter()
                .all(|(other, active)| *other == item || !*active));
            pages.insert(item.page_name());
            keys.insert(item.label_key());
        }
        assert_eq!(pages.len(), NavItem::ALL.len());
        assert_eq!(keys.len(), NavItem::ALL.len());
        for key in keys {
            for locale in [
                presentation::DEFAULT_LOCALE,
                presentation::FALLBACK_LANGUAGE,
            ] {
                let text = rust_i18n::t!(key, locale = locale).to_string();
                assert_ne!(text, key, "{locale} 缺少导航文案键 {key}");
                assert!(!text.trim().is_empty());
            }
        }
    }

    /// 锚点（§10）：锁定状态徽章与设备态一一对应。
    #[test]
    fn test_lock_badge_matches_device_state() {
        /// 徽章允许使用的内置语义类全集(libadwaita/GTK 自带;仓库内无自定义样式表)。
        const BUILTIN_BADGE_CLASSES: [&str; 3] = ["warning", "success", "dim-label"];
        assert_eq!(badge_class(DeviceIdentity::Locked), "warning");
        assert_eq!(badge_class(DeviceIdentity::Unlocked), "success");
        assert_eq!(badge_class(DeviceIdentity::ReEnumerating), "dim-label");
        assert_eq!(badge_class(DeviceIdentity::Unrecognized), "dim-label");
        // 无设备(非目标 PID → 未识别)落到弱化徽章。
        let no_device = DeviceIdentity::from_ids(magi_protocol::VENDOR_ID, 0x61ff);
        assert_eq!(no_device, DeviceIdentity::Unrecognized);
        assert_eq!(badge_class(no_device), "dim-label");
        // 锁定态与解锁态必须用不同徽章(醒目区分)。
        assert_ne!(
            badge_class(DeviceIdentity::Locked),
            badge_class(DeviceIdentity::Unlocked)
        );
        // 全部身份的徽章类都必须是内置语义类(不依赖任何自定义样式表)。
        for identity in [
            DeviceIdentity::Locked,
            DeviceIdentity::Unlocked,
            DeviceIdentity::ReEnumerating,
            DeviceIdentity::Unrecognized,
        ] {
            assert!(
                BUILTIN_BADGE_CLASSES.contains(&badge_class(identity)),
                "{identity:?} 的徽章类必须是内置语义类"
            );
        }
    }

    /// 进度文案键与 7 个步骤一一对应（§4.13：文案经 i18n 键渲染）。
    #[test]
    fn test_progress_keys_resolve_for_all_steps() {
        assert_eq!(UnlockStep::ALL.len(), 7);
        for step in UnlockStep::ALL {
            let key = format!("progress.{step}");
            for locale in [
                presentation::DEFAULT_LOCALE,
                presentation::FALLBACK_LANGUAGE,
            ] {
                let text = rust_i18n::t!(key.as_str(), locale = locale).to_string();
                assert_ne!(text, key, "{locale} 缺少 {key}");
            }
        }
    }

    /// 从模板 XML 收集 `id="…"` 取值。
    fn collect_ids(xml: &str) -> Vec<String> {
        let mut ids = Vec::new();
        let mut rest = xml;
        while let Some(position) = rest.find("id=\"") {
            rest = &rest[position + 4..];
            if let Some(end) = rest.find('"') {
                ids.push(rest[..end].to_string());
                rest = &rest[end..];
            }
        }
        ids
    }

    /// 取某个 `<property name="…">VALUE</property>` 的取值（本项目模板里该属性至多出现一次）。
    fn property_value(xml: &str, name: &str) -> Option<String> {
        let needle = format!("name=\"{name}\">");
        let start = xml.find(&needle)? + needle.len();
        let end = xml[start..].find('<')? + start;
        Some(xml[start..end].trim().to_string())
    }

    /// `.ui` 只承载结构（K5/AC-015）：不含中日韩文字，且显示类属性不出现非空字面量。
    ///
    /// XML 注释不参与界面呈现，先剥离（模板注释本身是中文维护说明）。
    fn assert_no_display_literals(xml: &str) {
        let xml = strip_comments(xml);
        assert!(
            !xml.chars()
                .any(|character| ('\u{4e00}'..='\u{9fff}').contains(&character)),
            "模板内不得出现中文（文案必须走 i18n 键）"
        );
        for attribute in [
            "label=\"",
            "title=\"",
            "subtitle=\"",
            "text=\"",
            "tooltip-text=\"",
            "placeholder-text=\"",
        ] {
            assert!(
                !xml.contains(attribute),
                "模板内不得内联显示文案（命中 {attribute}）"
            );
        }
        for property in [
            "label",
            "title",
            "subtitle",
            "text",
            "tooltip-text",
            "placeholder-text",
        ] {
            if let Some(value) = property_value(&xml, property) {
                assert!(
                    value.is_empty(),
                    "属性 {property} 不得带字面量取值：{value:?}"
                );
            }
        }
    }

    /// 剥离 XML 注释。
    fn strip_comments(xml: &str) -> String {
        let mut output = String::with_capacity(xml.len());
        let mut rest = xml;
        while let Some(start) = rest.find("<!--") {
            output.push_str(&rest[..start]);
            match rest[start..].find("-->") {
                Some(end) => rest = &rest[start + end + 3..],
                None => return output,
            }
        }
        output.push_str(rest);
        output
    }

    /// 模板装载（K6）：`gtk::init()` 失败时打印跳过原因并返回，不 `#[ignore]`。
    #[test]
    fn test_main_window_instantiates_with_template() {
        if !crate::test_support::gtk_ready("test_main_window_instantiates_with_template") {
            return;
        }
        let window = MainWindow::new();
        let imp = window.imp();
        // 契约的五个子件都必须由 .ui 解析到（id 不匹配会 panic，类型不符会被 GType 断言拦下）。
        assert_eq!(imp.device_card.get().type_().name(), "AdwBin");
        assert_eq!(imp.status_label.get().type_().name(), "GtkLabel");
        assert_eq!(imp.action_unlock.get().type_().name(), "GtkButton");
        assert_eq!(imp.progress.get().type_().name(), "GtkProgressBar");
        assert_eq!(imp.result_label.get().type_().name(), "GtkLabel");

        // 启动态：设备未识别 → 全部入口禁用（§4.1）。
        for action in ActionId::ALL {
            let button = window.action_button(action).expect("入口按钮必须存在");
            assert!(!button.is_sensitive(), "{action:?} 启动态必须禁用");
            assert!(!button.label().unwrap_or_default().is_empty());
        }

        // 锁定态：解锁/校验可用，写口令入口仍禁用（§4.11）。
        window.apply_actions(DeviceIdentity::Locked, &UnlockGate::new());
        assert!(window
            .action_button(ActionId::Unlock)
            .unwrap()
            .is_sensitive());
        assert!(window
            .action_button(ActionId::ValidatePassword)
            .unwrap()
            .is_sensitive());
        for action in [
            ActionId::SetPassword,
            ActionId::ChangePassword,
            ActionId::DeletePassword,
        ] {
            assert!(!window.action_button(action).unwrap().is_sensitive());
        }

        // 错误呈现：呈现码 + 原因 + 建议（§4.13）。
        let text = window.show_error(&presentation::AppError::EmptyPassword);
        assert!(text.starts_with("EmptyPassword"));
        assert!(text.contains(&t!("EmptyPassword.reason").to_string()));
        assert!(text.contains(&t!("EmptyPassword.advice").to_string()));
    }
}
