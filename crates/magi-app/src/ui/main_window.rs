//! §4.12 主窗口（D29：界面全部由 Rust 代码构建，使用 libadwaita 原生组件与标准页面模式）。
//!
//! 结构：`AdwToolbarView` + `AdwHeaderBar`（含首选项入口与侧边栏开关）+
//! `AdwOverlaySplitView`（侧边栏 = 挂内置 `.navigation-sidebar` 类的 `GtkListBox`；主区 =
//! `GtkStack`，三页均为 `AdwPreferencesPage`）。
//!
//! 页面组织（D29 分文件）：三页的控件构建与页面级呈现分文件维护——[`crate::ui::dashboard`] /
//! [`crate::ui::diagnostics_page`] / [`crate::ui::about_page`]；本文件保留窗口骨架、装配、
//! 导航路由与作业/口令/环境编排。
//!
//! 仪表盘（D29）：设备分组（`AdwPreferencesGroup` + `AdwActionRow`：产品名 / VID:PID /
//! 设备节点 / 传输通道 / USB 备用设置，锁定状态徽章置于设备行尾部）+ 操作分组
//! （五个 `AdwButtonRow`）+ 进度与结果分组；诊断页 = 只读记录卡片 + 脱敏导出入口；
//! 关于页 = 版本 / 支持设备 / 协议参考三行。
//!
//! 文案（K5/AC-015）：本文件不内联任何可显示字符串，全部经 i18n 键在装配时赋值。
//!
//! 主流程（§4.13）：启动即扫描设备 → 更新设备分组/状态徽章/入口 → 用户触发 → 口令对话框 →
//! 工作线程作业 → `AppEvent` 回主线程更新进度与结果；取消只停止后续步骤，不阻塞退出（§6）。
//! 设备热插拔（§4.1 REQ-001）：工作线程周期重扫，经 MainContext channel 回主线程，
//! 由重扫状态机（[`crate::device::rescan::RescanState`]）统一裁决设备分组与入口更新。

use std::cell::RefCell;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use magi_protocol::{Password, UnlockEvidence};
#[cfg(test)]
use magi_protocol::UnlockStep;
use rust_i18n::t;

use crate::diagnostics::{self, Level};
use crate::device::environment::{self, EnvironmentEvent, EnvironmentState};
use crate::device::gate::{ActionId, UnlockGate};
use crate::device::jobs::{self, CancelFlag, DeviceJob, ScanHit};
use crate::device::rescan::{DeviceIdentity, RescanAction, RescanState};
use crate::ui::about_page;
use crate::ui::dashboard;
use crate::ui::diagnostics_page;
use crate::ui::environment_dialog::EnvironmentDialog;
use crate::ui::password_dialog::PasswordDialog;
use crate::presentation::{self, AppError, AppEvent};

/// 侧边栏导航项（对标原版 Magician 的分组导航列表；选中态恒唯一）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavItem {
    /// 仪表盘：设备分组、操作入口、进度与结果。
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

    /// 导航项标签与图标的 i18n 键。
    pub fn label_key(self) -> &'static str {
        match self {
            NavItem::Dashboard => "nav.dashboard",
            NavItem::Diagnostics => "nav.diagnostics",
            NavItem::About => "nav.about",
        }
    }

    /// 导航项图标（Adwaita 图标主题自带 symbolic 图标）。
    pub fn icon_name(self) -> &'static str {
        match self {
            NavItem::Dashboard => "view-grid-symbolic",
            NavItem::Diagnostics => "view-list-symbolic",
            NavItem::About => "help-about-symbolic",
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

/// 徽章图标（语义与徽章类一一对应，全部为 Adwaita 图标主题自带 symbolic 图标）。
pub fn status_icon_name(identity: DeviceIdentity) -> &'static str {
    match identity {
        DeviceIdentity::Locked => "changes-prevent-symbolic",
        DeviceIdentity::Unlocked => "emblem-ok-symbolic",
        DeviceIdentity::ReEnumerating => "view-refresh-symbolic",
        DeviceIdentity::Unrecognized => "dialog-question-symbolic",
    }
}

/// 结果区语义分级（内置类）：中性（弱化文本，无图标）、成功（success + emblem-ok）、
/// 错误（error + dialog-warning）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// 中性：进度步骤、取消、未观察到重枚举等。
    Neutral,
    /// 成功：判据确认或口令被接受。
    Success,
    /// 错误：操作失败（红色分级）。
    Error,
}

/// 结果键 → 语义分级：判据确认/口令被接受为成功；导出失败为错误；其余为中性。
pub(crate) fn result_kind(key: &str) -> Outcome {
    match key {
        "result.RealPartitionTable" | "result.LockingFlags" | "result.validate_accepted" => {
            Outcome::Success
        }
        "diagnostics.export_failed" => Outcome::Error,
        _ => Outcome::Neutral,
    }
}

/// 结果区呈现的重放数据（relocalize 用）。
#[derive(Debug, Clone)]
pub(crate) enum StoredOutcome {
    /// 固定结果键。
    Result(&'static str),
    /// 已解锁并挂载（含挂载点参数）。
    Mounted(Vec<String>),
    /// 错误（呈现码 + 原因/建议键由 AppError 推导）。
    Error(AppError),
}

/// 主窗口默认尺寸裁决（D31）：按窗口所在显示器的工作区推导，含上下限；不写死像素常数。
///
/// 比例与上下限是呈现细节（spec 不落数值）：宽取工作区宽度的 3/5（夹逼 720–1200），
/// 高取工作区高度的 7/10（夹逼 600–900）。同一工作区恒定同尺寸，显示密度/字体缩放
/// 差异由「相对工作区」而非绝对像素吸收；无显示器（无头环境）由调用方回落。
pub fn compute_default_size(workarea: (i32, i32)) -> (i32, i32) {
    let width = (workarea.0 * 3 / 5).clamp(720, 1200);
    let height = (workarea.1 * 7 / 10).clamp(600, 900);
    (width, height)
}

/// 当前显示器的工作区尺寸（无头环境返回 None）。
fn current_workarea() -> Option<(i32, i32)> {
    let display = gtk::gdk::Display::default()?;
    // GDK4 无 primary 概念（Wayland），取显示器列表首项作为推导基准。
    let monitor = display
        .monitors()
        .item(0)
        .and_then(|object| object.downcast::<gtk::gdk::Monitor>().ok())?;
    let geometry = monitor.geometry();
    Some((geometry.width(), geometry.height()))
}

#[derive(Debug, Default)]
pub(crate) struct WindowState {
    pub(crate) gate: UnlockGate,
    pub(crate) device: Option<DeviceJob>,
    pub(crate) cancel: CancelFlag,
    pub(crate) action: Option<ActionId>,
    /// 周期重扫状态机：呈现中身份、在位缓存与空态防抖计数。
    pub(crate) rescan: RescanState,
    /// 结果区重放缓存（语言切换后按新语言重放）。
    pub(crate) last_outcome: Option<StoredOutcome>,
    /// 最后一次设备扫描命中（relocalize 重放设备分组文案用；空态清除）。
    pub(crate) last_hit: Option<ScanHit>,
    /// 打开中的口令对话框（语言切换时同步重渲染）。
    pub(crate) open_dialog: Option<glib::WeakRef<PasswordDialog>>,
    /// 打开中的引导向导（环境状态迁移时刷新问题清单）。
    pub(crate) open_environment_dialog: Option<glib::WeakRef<EnvironmentDialog>>,
    /// 环境就绪状态机当前态（§4.13）。
    pub(crate) environment: EnvironmentState,
    /// 启动引导链一次性自动动作（D33）：装载/修复各至多一次，收口进状态机。
    pub(crate) env_autos: crate::device::environment::AutoAttempts,
}

mod imp {
    use super::*;

    /// 主窗口子件（D29）：全部在 Rust 侧构建，无 `.ui` 模板、无 `#[template_child]`。
    pub struct MainWindow {
        pub root: adw::ToolbarView,
        pub sidebar_toggle: gtk::ToggleButton,
        pub action_preferences: gtk::Button,
        pub environment_pill: gtk::Button,
        pub window_title: adw::WindowTitle,
        pub sidebar_title: gtk::Label,
        pub split_view: adw::OverlaySplitView,
        pub nav_list: gtk::ListBox,
        pub nav_labels: Vec<gtk::Label>,
        pub content_stack: gtk::Stack,
        pub(crate) dashboard: dashboard::DashboardWidgets,
        pub(crate) diagnostics: diagnostics_page::DiagnosticsWidgets,
        pub(crate) about: about_page::AboutWidgets,
        pub(crate) state: RefCell<WindowState>,
    }

    impl Default for MainWindow {
        fn default() -> Self {
            Self::build()
        }
    }

    impl MainWindow {
        /// 构建完整界面树（D29：代码构建，不使用 `.ui` 模板）。
        fn build() -> Self {
            // —— 顶栏：侧边栏开关（start）、标题（中间）、首选项入口（end）——
            let sidebar_toggle = gtk::ToggleButton::builder()
                .icon_name("sidebar-show-symbolic")
                .build();
            let window_title = adw::WindowTitle::new("", "");
            let action_preferences = gtk::Button::builder()
                .icon_name("emblem-system-symbolic")
                .build();
            // 环境胶囊（D30）：警示色（warning 调色 + suggested-action 背景），
            // 环境问题存在时呈现；问题清单与修复动作在引导向导中。
            let environment_pill = gtk::Button::builder()
                .icon_name("dialog-warning-symbolic")
                .visible(false)
                .build();
            environment_pill.add_css_class("suggested-action");
            environment_pill.add_css_class("warning");
            let header = adw::HeaderBar::new();
            header.pack_start(&sidebar_toggle);
            header.set_title_widget(Some(&window_title));
            header.pack_end(&action_preferences);
            header.pack_end(&environment_pill);

            // —— 侧边栏：原生 flat HeaderBar 品牌（关闭标题按钮绘制——macOS 下
            // 避免与主标题栏重复渲染窗口控制圆点；尺寸由 libadwaita 统一）——
            // 品牌标签：比导航项明显大一号（title-2），置于原生 flat 标题栏内。
            let sidebar_title = gtk::Label::new(None);
            sidebar_title.add_css_class("title-2");
            let sidebar_header = adw::HeaderBar::new();
            sidebar_header.add_css_class("flat");
            sidebar_header.set_show_start_title_buttons(false);
            sidebar_header.set_show_end_title_buttons(false);
            sidebar_header.set_title_widget(Some(&sidebar_title));

            let nav_list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::Single)
                .build();
            nav_list.add_css_class("navigation-sidebar");
            let mut nav_labels = Vec::with_capacity(NavItem::ALL.len());
            for item in NavItem::ALL {
                let icon = gtk::Image::from_icon_name(item.icon_name());
                let label = gtk::Label::builder().xalign(0.0).build();
                let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                row_box.set_margin_top(8);
                row_box.set_margin_bottom(8);
                row_box.set_margin_start(6);
                row_box.set_margin_end(6);
                row_box.append(&icon);
                row_box.append(&label);
                let row = gtk::ListBoxRow::new();
                row.set_child(Some(&row_box));
                nav_list.append(&row);
                nav_labels.push(label);
            }

            let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
            sidebar_box.set_margin_top(12);
            sidebar_box.set_margin_bottom(12);
            sidebar_box.set_margin_start(12);
            sidebar_box.set_margin_end(12);
            sidebar_box.append(&nav_list);

            let sidebar_view = adw::ToolbarView::new();
            sidebar_view.add_top_bar(&sidebar_header);
            sidebar_view.set_content(Some(&sidebar_box));

            // —— 页面栈：仪表盘 / 诊断 / 关于（每页一个 AdwPreferencesPage）——
            let content_stack = gtk::Stack::builder()
                .transition_type(gtk::StackTransitionType::Crossfade)
                .transition_duration(200)
                .hexpand(true)
                .vexpand(true)
                .build();

            // 仪表盘页（D29 分文件）：构建与设备分组呈现见 dashboard.rs；
            // 页面根 = 包裹 PreferencesPage 的 ToastOverlay。
            let (dash_root, dashboard) = dashboard::build();
            content_stack.add_named(&dash_root, Some(NavItem::Dashboard.page_name()));

            // 诊断页（D29 分文件）：构建与刷新见 diagnostics_page.rs。
            let (diag_root, diagnostics) = diagnostics_page::build();
            content_stack.add_named(&diag_root, Some(NavItem::Diagnostics.page_name()));

            // 关于页（D29 分文件）：构建与平台说明见 about_page.rs。
            let (about_root, about) = about_page::build();
            content_stack.add_named(&about_root, Some(NavItem::About.page_name()));

            // —— 分栏视图与根容器 ——
            let split_view = adw::OverlaySplitView::builder()
                .min_sidebar_width(240.0)
                .max_sidebar_width(280.0)
                .sidebar(&sidebar_view)
                .content(&content_stack)
                .build();

            let root = adw::ToolbarView::new();
            root.add_top_bar(&header);
            root.set_content(Some(&split_view));

            Self {
                root,
                sidebar_toggle,
                action_preferences,
                environment_pill,
                window_title,
                sidebar_title,
                split_view,
                nav_list,
                nav_labels,
                content_stack,
                dashboard,
                diagnostics,
                about,
                state: RefCell::new(WindowState::default()),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MainWindow {
        const NAME: &'static str = "MagiMainWindow";
        type Type = super::MainWindow;
        type ParentType = adw::ApplicationWindow;
    }

    impl ObjectImpl for MainWindow {
        fn constructed(&self) {
            self.parent_constructed();
            // 构建期完成界面树的挂载（D29：无 GtkBuilder 模板，全部代码构建）。
            let window = self.obj();
            <super::MainWindow as adw::prelude::AdwApplicationWindowExt>::set_content(
                &window,
                Some(&self.root),
            );
        }
    }

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
    /// 构造主窗口：构建界面树、装配文案与初始入口状态。
    pub fn new() -> Self {
        let window: Self = glib::Object::new();
        // D31：默认尺寸按显示器工作区推导；无头环境（测试）回落 1024×700。
        let (width, height) = current_workarea()
            .map(compute_default_size)
            .unwrap_or((1024, 700));
        window.set_default_size(width, height);
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
        self.start_environment_bootstrap();
        self.refresh_devices();
        self.refresh_actions();
        self.spawn_device_watch();
    }

    /// 文案与初始状态（K5/AC-015：全部经 i18n 键赋值，代码内不内联可显示字符串）。
    fn setup(&self) {
        let imp = self.imp();
        imp.sidebar_title.set_label(&t!("app.title"));
        imp.window_title.set_title(&t!(NavItem::Dashboard.label_key()));
        for (label, item) in imp.nav_labels.iter().zip(NavItem::ALL) {
            label.set_label(&t!(item.label_key()));
        }
        imp.content_stack
            .set_visible_child_name(NavItem::Dashboard.page_name());
        // 初始选中第一项：选中态由 GtkListBox 原生机制维护（此时导航回调尚未接线，
        // 不会重入 show_page；页面可见性已由上一行直接设置）。
        if let Some(row) = imp.nav_list.row_at_index(0) {
            imp.nav_list.select_row(Some(&row));
        }
        imp.dashboard.device_group.set_title(&t!("device.group_title"));
        imp.dashboard.actions_group.set_title(&t!("dashboard.actions_title"));
        imp.dashboard.actions_group
            .set_description(Some(&t!("reason.evidence_gap")));
        imp.dashboard.feedback_group
            .set_title(&t!("dashboard.feedback_title"));
        imp.dashboard.node_row.set_title(&t!("device.node"));
        imp.dashboard.channel_row.set_title(&t!("device.channel_title"));
        imp.dashboard.descriptor_row.set_title(&t!("device.descriptor_title"));
        imp.diagnostics.diagnostics_group
            .set_title(&t!("diagnostics.view_title"));
        imp.diagnostics.diagnostics_group
            .set_description(Some(&t!("diagnostics.hint")));
        imp.about.about_group.set_title(&t!(NavItem::About.label_key()));
        imp.about.about_group
            .set_description(Some(&t!("about.notice")));
        imp.about.about_version_row.set_title(&t!("about.version_title"));
        imp.about.about_device_row.set_title(&t!("about.device_title"));
        imp.about.about_repository_row
            .set_title(&t!("about.repository_title"));
        imp.about.platform_group
            .set_title(&t!("about.platform_title"));
        imp.about.about_version_row
            .set_subtitle(env!("CARGO_PKG_VERSION"));
        imp.about.about_device_row.set_subtitle(t!("app.subtitle").as_ref());
        imp.about.about_repository_row
            .set_subtitle(t!("about.repository").as_ref());
        for action in ActionId::ALL {
            if let Some(row) = self.action_row(action) {
                row.set_title(&t!(action.label_key()));
            }
        }
        imp.diagnostics.action_export_diagnostics
            .set_title(&t!("action.export_diagnostics"));
        imp.dashboard.action_cancel.set_label(&t!("action.cancel"));
        imp.action_preferences
            .set_tooltip_text(Some(&t!("action.preferences")));
        imp.environment_pill
            .set_label(&t!("environment.pill_label"));
        imp.environment_pill
            .set_tooltip_text(Some(&t!("environment.pill_tooltip")));
        imp.sidebar_toggle
            .set_tooltip_text(Some(&t!("nav.toggle_sidebar")));
        // 侧边栏开关常显：桌面宽度也应能收起侧边栏（ GNOME 应用惯例）。
        // 同步方向以 split_view 为源：初始 sync_create 把 show-sidebar(true) 推给
        // 开关的 active，避免以开关默认 false 反向把侧边栏在启动时关掉。
        imp.split_view
            .bind_property("show-sidebar", &imp.sidebar_toggle, "active")
            .bidirectional()
            .sync_create()
            .build();
        self.hide_outcome();
        self.show_unknown_device();
        self.apply_actions(DeviceIdentity::Unrecognized, &UnlockGate::new());
    }

    /// 入口接线（§4.12/§4.11/§6）。
    fn connect_actions(&self) {
        let imp = self.imp();
        for action in ActionId::ALL {
            let Some(row) = self.action_row(action) else {
                continue;
            };
            let this = self.clone();
            row.connect_activated(glib::clone!(
                #[weak]
                this,
                move |_| this.trigger(action)
            ));
        }
        let this = self.clone();
        imp.dashboard.action_cancel.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.cancel_current()
        ));
        let this = self.clone();
        imp.diagnostics.action_export_diagnostics.connect_activated(glib::clone!(
            #[weak]
            this,
            move |_| this.export_diagnostics()
        ));
        let this = self.clone();
        imp.action_preferences.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.present_preferences()
        ));
        let this = self.clone();
        imp.environment_pill.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.present_environment_dialog()
        ));
        let this = self.clone();
        imp.nav_list.connect_row_selected(glib::clone!(
            #[weak]
            this,
            move |_, row| {
                // 选中行序与 `NavItem::ALL` 一致；点击行时 GTK 自动选中 → 此处切页。
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
    /// 呈现）；扫描阶段的失败只记诊断（呈现码进诊断导出）——设备分组空态与侧边栏平台
    /// 说明已给出路，结果区留给用户触发的作业呈现（与周期轮次同口径）。
    fn refresh_devices(&self) {
        self.show_platform_notice();
        let outcome = jobs::fetch_scan();
        if let Some(error) = &outcome.error {
            diagnostics::ring().record(Level::Warn, presentation::presentation_code(error));
        }
        self.apply_scan(&outcome, true);
    }

    /// 呈现段（主线程）：按重扫状态机裁决并更新设备分组、徽章与入口。
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
            let Some(row) = self.action_row(action) else {
                continue;
            };
            let enabled = gate.allows(action, identity.state());
            let reason_key = gate.disabled_reason_key(action, identity.state());
            row.set_sensitive(enabled);
            row.set_tooltip_text(reason_key.map(|key| t!(key).to_string()).as_deref());
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

    /// 窗口运行期状态（设备、闸门、取消标志与在飞动作）。
    pub(crate) fn state(&self) -> &RefCell<WindowState> {
        &self.imp().state
    }

    /// 切换页面并同步导航选中态（选中唯一性由 `GtkListBox` 原生选中机制保证）。
    fn show_page(&self, item: NavItem) {
        let imp = self.imp();
        imp.content_stack.set_visible_child_name(item.page_name());
        imp.window_title.set_title(&t!(item.label_key()));
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

    /// 打开设置对话框（D28）：装入当前设置快照；更改即时生效并回写本窗口。
    fn present_preferences(&self) {
        let settings = crate::settings::Settings::load();
        let dialog = crate::ui::settings_dialog::SettingsDialog::new(self, settings);
        dialog.present(Some(self));
    }

    /// 环境就绪引导装配（§4.13）：启动自检 → 状态机裁决 → 胶囊/装载动作。
    fn start_environment_bootstrap(&self) {
        let report = environment::inspect_environment();
        self.apply_environment_event(EnvironmentEvent::CheckDone(report));
    }

    /// 入口行（按 `ActionId` 取操作分组的 `AdwButtonRow`）。
    pub fn action_row(&self, action: ActionId) -> Option<adw::ButtonRow> {
        let imp = self.imp();
        Some(match action {
            ActionId::Unlock => imp.dashboard.action_unlock.clone(),
            ActionId::ValidatePassword => imp.dashboard.action_validate_password.clone(),
            ActionId::SetPassword => imp.dashboard.action_set_password.clone(),
            ActionId::ChangePassword => imp.dashboard.action_change_password.clone(),
            ActionId::DeletePassword => imp.dashboard.action_delete_password.clone(),
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

        // relocalize 联动缓存：语言切换时同步重渲染打开中的对话框（弱引用，不阻止回收）。
        self.state().borrow_mut().open_dialog = Some(dialog.downgrade());
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
                // relocalize 重放缓存：挂载点参数不走固定结果键，单独缓存。
                self.state().borrow_mut().last_outcome =
                    Some(StoredOutcome::Mounted(mounted.clone()));
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

    /// 语言切换后重设全部静态文案并重放动态呈现（D28：切换即时生效）。
    pub fn relocalize(&self) {
        let imp = self.imp();
        imp.sidebar_title.set_label(&t!("app.title"));
        imp.window_title.set_title(&t!(NavItem::Dashboard.label_key()));
        for (label, item) in imp.nav_labels.iter().zip(NavItem::ALL) {
            label.set_label(&t!(item.label_key()));
        }
        imp.dashboard.device_group.set_title(&t!("device.group_title"));
        imp.dashboard.actions_group.set_title(&t!("dashboard.actions_title"));
        imp.dashboard.actions_group
            .set_description(Some(&t!("reason.evidence_gap")));
        imp.dashboard.feedback_group
            .set_title(&t!("dashboard.feedback_title"));
        imp.dashboard.node_row.set_title(&t!("device.node"));
        imp.dashboard.channel_row.set_title(&t!("device.channel_title"));
        imp.dashboard.descriptor_row.set_title(&t!("device.descriptor_title"));
        imp.diagnostics.diagnostics_group
            .set_title(&t!("diagnostics.view_title"));
        imp.diagnostics.diagnostics_group
            .set_description(Some(&t!("diagnostics.hint")));
        imp.about.about_group.set_title(&t!(NavItem::About.label_key()));
        imp.about.about_group
            .set_description(Some(&t!("about.notice")));
        imp.about.about_version_row.set_title(&t!("about.version_title"));
        imp.about.about_device_row.set_title(&t!("about.device_title"));
        imp.about.about_repository_row
            .set_title(&t!("about.repository_title"));
        imp.about.platform_group
            .set_title(&t!("about.platform_title"));
        imp.about.about_device_row.set_subtitle(t!("app.subtitle").as_ref());
        imp.about.about_repository_row
            .set_subtitle(t!("about.repository").as_ref());
        for action in ActionId::ALL {
            if let Some(row) = self.action_row(action) {
                row.set_title(&t!(action.label_key()));
            }
        }
        imp.diagnostics.action_export_diagnostics
            .set_title(&t!("action.export_diagnostics"));
        imp.dashboard.action_cancel.set_label(&t!("action.cancel"));
        imp.action_preferences
            .set_tooltip_text(Some(&t!("action.preferences")));
        imp.environment_pill
            .set_label(&t!("environment.pill_label"));
        imp.environment_pill
            .set_tooltip_text(Some(&t!("environment.pill_tooltip")));
        imp.sidebar_toggle
            .set_tooltip_text(Some(&t!("nav.toggle_sidebar")));
        // 设备分组重放：有缓存命中按新语言重渲染（不走 refresh_devices——
        // RescanState 身份不变时返回 Keep，不会重刷文案）。
        let last_hit = self.state().borrow().last_hit.clone();
        match last_hit {
            Some(hit) => self.show_hit(&hit),
            None => self.show_unknown_device(),
        }
        self.show_platform_notice();
        self.refresh_actions();
        // 结果区重放。
        let last_outcome = self.state().borrow().last_outcome.clone();
        match last_outcome {
            Some(StoredOutcome::Result(key)) => self.show_result(key),
            Some(StoredOutcome::Mounted(volumes)) => {
                let text = format!(
                    "{} {}",
                    t!("result.RealPartitionTable"),
                    t!("result.mounted", volumes = volumes.join(", "))
                );
                self.show_outcome(&text, Outcome::Success);
            }
            Some(StoredOutcome::Error(error)) => {
                self.show_error(&error);
            }
            None => self.hide_outcome(),
        }
        let current = imp.content_stack.visible_child_name().unwrap_or_default();
        if current.as_str() == NavItem::Diagnostics.page_name() {
            self.refresh_diagnostics_view();
        }
        let dialog = self
            .state()
            .borrow()
            .open_dialog
            .as_ref()
            .and_then(|weak| weak.upgrade());
        if let Some(dialog) = dialog {
            dialog.relocalize();
        }
        let env_dialog = self
            .state()
            .borrow()
            .open_environment_dialog
            .as_ref()
            .and_then(|weak| weak.upgrade());
        if let Some(dialog) = env_dialog {
            dialog.relocalize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation;

    /// 锚点（§10）：侧边栏导航 ≥3 项且选中态唯一。
    #[test]
    fn test_sidebar_navigation_items() {
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

        // 结构（D29：代码构建）：导航列表 ≥3 行、挂内置类、三个页名齐备、选中态唯一。
        if !crate::test_support::gtk_ready("test_sidebar_navigation_items") {
            return;
        }
        let window = MainWindow::new();
        let imp = window.imp();
        let row_count = nav_row_count(&imp.nav_list);
        assert!(row_count >= 3, "导航项至少 3 项，实际 {row_count}");
        assert!(
            imp.nav_list.has_css_class("navigation-sidebar"),
            "侧边栏导航必须用内置 navigation-sidebar 类"
        );
        for item in NavItem::ALL {
            assert!(
                imp.content_stack
                    .child_by_name(item.page_name())
                    .is_some(),
                "页面栈缺页 {}",
                item.page_name()
            );
        }
        let selected = (0..row_count)
            .filter_map(|index| imp.nav_list.row_at_index(index))
            .filter(|row| row.is_selected())
            .count();
        assert_eq!(selected, 1, "任一时刻选中项必须恰 1 个");
    }

    /// 导航列表行数（`GtkListBox` 无直接计数 API，按索引探测）。
    fn nav_row_count(list: &gtk::ListBox) -> i32 {
        let mut count = 0;
        while list.row_at_index(count).is_some() {
            count += 1;
        }
        count
    }

    /// 锚点（§10）：窗口默认尺寸随工作区推导、含上下限（D31）。
    #[test]
    fn test_window_size_scales_with_display() {
        // 单调：更大的工作区给出更大的默认窗口。
        let small = compute_default_size((1366, 768));
        let medium = compute_default_size((1920, 1080));
        let large = compute_default_size((2560, 1440));
        assert!(medium.0 > small.0 && large.0 > medium.0);
        assert!(medium.1 > small.1 && large.1 > medium.1);
        // 上下限：4K 收敛到与 2560×1440 相同的上限；小屏抬到下限。
        let huge = compute_default_size((7680, 4320));
        assert_eq!(huge, large, "超出上限的工作区必须收敛到同一上限");
        let tiny = compute_default_size((800, 600));
        assert!(tiny.0 >= 720 && tiny.1 >= 600, "小屏不得低于下限：{tiny:?}");
        // 确定性：同一工作区恒定同尺寸。
        assert_eq!(compute_default_size((1920, 1080)), medium);
    }

    /// 锚点（§10）：HeaderBar 环境胶囊存在、警示样式、就绪时隐藏（D30）。
    #[test]
    fn test_environment_pill_present() {
        // 结构（D29 + D30）：胶囊在 HeaderBar 末端、警示色语义类、初始隐藏、文案经 i18n。
        if !crate::test_support::gtk_ready("test_environment_pill_present") {
            return;
        }
        let window = MainWindow::new();
        let imp = window.imp();
        // 警示色 = warning 调色（libadwaita 内置类）+ suggested-action 背景。
        assert!(
            imp.environment_pill.has_css_class("warning"),
            "环境胶囊必须挂 warning 警示类"
        );
        assert!(
            imp.environment_pill.has_css_class("suggested-action"),
            "环境胶囊必须挂 suggested-action 背景类"
        );
        // 初始环境态 Unknown：胶囊不呈现（就绪口径同样隐藏）。
        assert!(
            !imp.environment_pill.is_visible(),
            "环境未发现问题时胶囊必须隐藏"
        );
        assert!(
            imp.environment_pill
                .label()
                .is_some_and(|label| !label.is_empty()),
            "胶囊文案必须经 i18n 键装配"
        );
        for locale in [
            presentation::DEFAULT_LOCALE,
            presentation::FALLBACK_LANGUAGE,
        ] {
            let text = rust_i18n::t!("environment.pill_label", locale = locale).to_string();
            assert_ne!(text, "environment.pill_label", "{locale} 缺少胶囊文案");
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

    /// 主窗口装配（K6）：`gtk::init()` 失败时打印跳过原因并返回，不 `#[ignore]`。
    #[test]
    fn test_main_window_instantiates() {
        if !crate::test_support::gtk_ready("test_main_window_instantiates") {
            return;
        }
        let window = MainWindow::new();
        let imp = window.imp();
        // D29：界面由代码构建的 libadwaita 标准组件承载。
        assert_eq!(imp.dashboard.device_group.type_().name(), "AdwPreferencesGroup");
        assert_eq!(imp.dashboard.device_row.type_().name(), "AdwActionRow");
        assert_eq!(imp.dashboard.status_label.type_().name(), "GtkLabel");
        assert_eq!(imp.dashboard.action_unlock.type_().name(), "AdwButtonRow");
        assert_eq!(imp.dashboard.progress.type_().name(), "GtkProgressBar");
        assert_eq!(imp.dashboard.result_label.type_().name(), "GtkLabel");

        // 启动态：设备未识别 → 全部入口禁用（§4.1）。
        for action in ActionId::ALL {
            let row = window.action_row(action).expect("入口行必须存在");
            assert!(!row.is_sensitive(), "{action:?} 启动态必须禁用");
            assert!(!row.title().is_empty());
        }

        // 锁定态：解锁/校验可用，写口令入口仍禁用（§4.11）。
        window.apply_actions(DeviceIdentity::Locked, &UnlockGate::new());
        assert!(window.action_row(ActionId::Unlock).unwrap().is_sensitive());
        assert!(window
            .action_row(ActionId::ValidatePassword)
            .unwrap()
            .is_sensitive());
        for action in [
            ActionId::SetPassword,
            ActionId::ChangePassword,
            ActionId::DeletePassword,
        ] {
            assert!(!window.action_row(action).unwrap().is_sensitive());
        }

        // 错误呈现：呈现码 + 原因 + 建议（§4.13）。
        let text = window.show_error(&presentation::AppError::EmptyPassword);
        assert!(text.starts_with("EmptyPassword"));
        assert!(text.contains(&t!("EmptyPassword.reason").to_string()));
        assert!(text.contains(&t!("EmptyPassword.advice").to_string()));
    }

    /// 锚点（D28）：设置对话框可实例化，两个三态行齐备且默认选中态正确。
    #[test]
    fn test_settings_dialog_instantiates() {
        if !crate::test_support::gtk_ready("test_settings_dialog_instantiates") {
            return;
        }
        let window = MainWindow::new();
        let dialog = crate::ui::settings_dialog::SettingsDialog::new(&window, Default::default());
        let imp = dialog.imp();
        assert_eq!(imp.theme_row.type_().name(), "AdwComboRow");
        assert_eq!(imp.language_row.type_().name(), "AdwComboRow");
        // 默认深色主题选中第 3 行；默认语言跟随系统选中第 1 行。
        assert_eq!(imp.theme_row.selected(), 2);
        assert_eq!(imp.language_row.selected(), 0);
        // 两个三态行各 3 个候选项（模型在 relocalize 中装配）。
        let theme_model = imp.theme_row.model().expect("主题行必须有模型");
        assert_eq!(theme_model.n_items(), 3);
        let language_model = imp.language_row.model().expect("语言行必须有模型");
        assert_eq!(language_model.n_items(), 3);
        // 语言 autonym 键在两种界面语言下取值相同（D28：语言用自称呈现）。
        for key in ["settings.lang_zh", "settings.lang_en"] {
            let zh = rust_i18n::t!(key, locale = presentation::DEFAULT_LOCALE).to_string();
            let en = rust_i18n::t!(key, locale = presentation::FALLBACK_LANGUAGE).to_string();
            assert_eq!(zh, en, "{key} 的 autonym 必须语言无关");
        }
    }

    /// 步骤文案不得残留上一次结果的语义（成功后再操作：摘 success/error/dim-label
    /// 着色并隐藏结果图标，进度区保持可见）。
    #[test]
    fn test_show_step_clears_stale_outcome_classes() {
        if !crate::test_support::gtk_ready("test_show_step_clears_stale_outcome_classes") {
            return;
        }
        let window = MainWindow::new();
        // 先呈现一次成功结果（success 着色 + emblem-ok 图标）。
        window.show_result("result.RealPartitionTable");
        let imp = window.imp();
        assert!(imp.dashboard.result_label.has_css_class("success"));
        assert!(imp.dashboard.result_icon.is_visible());
        // 再进入下一次作业的步骤呈现：旧语义必须被摘除。
        window.show_step(UnlockStep::ALL[0]);
        for class in ["success", "error", "dim-label"] {
            assert!(
                !imp.dashboard.result_label.has_css_class(class),
                "步骤文案不得残留 {class}"
            );
        }
        assert!(!imp.dashboard.result_icon.is_visible());
        assert!(imp.dashboard.progress_box.is_visible());
        assert_eq!(
            imp.dashboard.result_label.label(),
            t!(format!("progress.{}", UnlockStep::ALL[0])).to_string()
        );
    }
}
