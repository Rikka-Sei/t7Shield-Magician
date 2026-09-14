//! §4.12 主窗口（D30：界面由 Rust 代码构建，libadwaita 原生组件）。
//!
//! 结构：`AdwToolbarView` + `AdwHeaderBar`（侧边栏开关、动态页名标题、首选项入口）+
//! `AdwOverlaySplitView`（侧边栏 = 原生标题栏品牌 + 挂 `.navigation-sidebar` 的 `GtkListBox`；
//! 主区 = `AdwToastOverlay` 包裹的仪表盘卡片布局与关于页）。
//!
//! 导航为两类页面（概览 / 关于，D30）：原诊断页移除，「导出工作日志」入口移入设置对话框。
//! 仪表盘：卡片式设备区（图标 + 产品名 + VID:PID + 锁定徽章 + 信息行）与操作卡
//! （解锁 / 验证口令主按钮 + 口令管理禁用组）+ 进度与结果反馈；成功结果经 `AdwToast`
//! 瞬时提示（libadwaita 特色）。
//!
//! 文案（K5/AC-015）：本文件不内联任何可显示字符串，全部经 i18n 键在装配时赋值。
//!
//! 主流程（§4.13）：启动即扫描设备 → 更新设备卡/状态/入口 → 用户触发 → 口令对话框 →
//! 工作线程作业 → `AppEvent` 回主线程更新进度与结果；取消只停止后续步骤，不阻塞退出（§6）。
//! 设备热插拔（§4.1 REQ-001）：工作线程周期重扫，经 MainContext channel 回主线程，
//! 由重扫状态机（[`crate::controller::RescanState`]）统一裁决设备区与入口更新。

use std::cell::RefCell;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use magi_protocol::{Password, UnlockEvidence, UnlockStep};
use rust_i18n::t;

use crate::controller::{ActionId, DeviceIdentity, RescanAction, RescanState, UnlockGate};
use crate::diagnostics::{self, Level};
use crate::jobs::{self, CancelFlag, DeviceJob, ScanHit};
use crate::ui::password_dialog::PasswordDialog;
use crate::presentation::{self, AppError, AppEvent};

/// 侧边栏导航项（D30：两类页面；选中态恒唯一）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavItem {
    /// 概览：设备卡、操作入口、进度与结果。
    Dashboard,
    /// 关于：版本与协议引用。
    About,
}

impl NavItem {
    /// 导航项全集（顺序即呈现顺序）。
    pub const ALL: [NavItem; 2] = [NavItem::Dashboard, NavItem::About];

    /// 页面栈中的页名。
    pub fn page_name(self) -> &'static str {
        match self {
            NavItem::Dashboard => "dashboard",
            NavItem::About => "about",
        }
    }

    /// 导航项标签与图标的 i18n 键。
    pub fn label_key(self) -> &'static str {
        match self {
            NavItem::Dashboard => "nav.dashboard",
            NavItem::About => "nav.about",
        }
    }

    /// 导航项图标（Adwaita 图标主题自带 symbolic 图标）。
    pub fn icon_name(self) -> &'static str {
        match self {
            NavItem::Dashboard => "view-grid-symbolic",
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

/// 结果区语义分级（内置类）：中性（弱化文本，无图标）、成功（success + emblem-ok + Toast）、
/// 错误（error + dialog-warning）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// 中性：进度步骤、取消、未观察到重枚举等。
    Neutral,
    /// 成功：判据确认或口令被接受（附加 Toast 瞬时提示）。
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
enum StoredOutcome {
    /// 固定结果键。
    Result(&'static str),
    /// 已解锁并挂载（含挂载点参数）。
    Mounted(Vec<String>),
    /// 错误（呈现码 + 原因/建议键由 AppError 推导）。
    Error(AppError),
}

#[derive(Debug, Default)]
pub(crate) struct WindowState {
    gate: UnlockGate,
    device: Option<DeviceJob>,
    cancel: CancelFlag,
    action: Option<ActionId>,
    /// 周期重扫状态机：呈现中身份、在位缓存与空态防抖计数。
    rescan: RescanState,
    /// 结果区重放缓存（语言切换后按新语言重放）。
    last_outcome: Option<StoredOutcome>,
    /// 最后一次设备扫描命中（relocalize 重放设备卡文案用；空态清除）。
    last_hit: Option<ScanHit>,
    /// 打开中的口令对话框（语言切换时同步重渲染）。
    open_dialog: Option<glib::WeakRef<PasswordDialog>>,
}

mod imp {
    use super::*;

    /// 主窗口子件（D29/D30：全部在 Rust 侧构建，无 `.ui` 模板）。
    pub struct MainWindow {
        pub root: adw::ToolbarView,
        pub sidebar_toggle: gtk::ToggleButton,
        pub action_preferences: gtk::Button,
        /// 主标题栏标题：随当前页动态切换（概览 / 关于）。
        pub window_title: adw::WindowTitle,
        /// 侧边栏品牌标题（原生标题栏控件，尺寸由 libadwaita 统一）。
        pub sidebar_title: adw::WindowTitle,
        pub split_view: adw::OverlaySplitView,
        pub nav_list: gtk::ListBox,
        pub nav_labels: Vec<gtk::Label>,
        pub content_stack: gtk::Stack,
        pub toast_overlay: adw::ToastOverlay,
        pub device_card: adw::Bin,
        pub device_icon: gtk::Image,
        pub device_model_label: gtk::Label,
        pub device_ids_label: gtk::Label,
        pub status_icon: gtk::Image,
        pub status_label: gtk::Label,
        pub badge_box: gtk::Box,
        pub node_caption: gtk::Label,
        pub node_label: gtk::Label,
        pub channel_caption: gtk::Label,
        pub channel_label: gtk::Label,
        pub descriptor_caption: gtk::Label,
        pub descriptor_label: gtk::Label,
        pub actions_card: adw::Bin,
        pub action_unlock: gtk::Button,
        pub action_validate_password: gtk::Button,
        pub action_set_password: gtk::Button,
        pub action_change_password: gtk::Button,
        pub action_delete_password: gtk::Button,
        pub password_admin_note: gtk::Label,
        pub progress_box: gtk::Box,
        pub progress: gtk::ProgressBar,
        pub action_cancel: gtk::Button,
        pub result_row: gtk::Box,
        pub result_icon: gtk::Image,
        pub result_label: gtk::Label,
        pub about_group: adw::PreferencesGroup,
        pub platform_group: adw::PreferencesGroup,
        pub about_version_row: adw::ActionRow,
        pub about_device_row: adw::ActionRow,
        pub about_repository_row: adw::ActionRow,
        pub platform_row: adw::ActionRow,
        pub(crate) state: RefCell<WindowState>,
    }

    impl Default for MainWindow {
        fn default() -> Self {
            Self::build()
        }
    }

    impl MainWindow {
        /// 构建完整界面树（D30：卡片式仪表盘 + 动态页名 + 两类页面）。
        fn build() -> Self {
            // —— 顶栏：侧边栏开关（start）、动态页名标题（中间）、首选项入口（end）——
            let sidebar_toggle = gtk::ToggleButton::builder()
                .icon_name("sidebar-show-symbolic")
                .build();
            let window_title = adw::WindowTitle::new("", "");
            let action_preferences = gtk::Button::builder()
                .icon_name("emblem-system-symbolic")
                .build();
            let header = adw::HeaderBar::new();
            header.pack_start(&sidebar_toggle);
            header.set_title_widget(Some(&window_title));
            header.pack_end(&action_preferences);

            // —— 侧边栏：原生标题栏品牌 + 导航列表（D30：两类页面）——
            let sidebar_title = adw::WindowTitle::new("", "");
            let sidebar_header = adw::HeaderBar::new();
            sidebar_header.add_css_class("flat");
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

            // —— 仪表盘（D30：卡片式布局，贴近官方软件的信息层级）——
            let device_icon = gtk::Image::from_icon_name("drive-harddisk-symbolic");
            device_icon.set_pixel_size(64);
            device_icon.add_css_class("dim-label");
            let status_icon = gtk::Image::builder().pixel_size(16).build();
            let status_label = gtk::Label::new(None);
            status_label.add_css_class("heading");
            let badge_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            badge_box.set_valign(gtk::Align::Center);
            badge_box.append(&status_icon);
            badge_box.append(&status_label);

            let device_model_label = gtk::Label::builder()
                .xalign(0.0)
                .halign(gtk::Align::Start)
                .wrap(true)
                .build();
            device_model_label.add_css_class("title-3");
            let device_ids_label = gtk::Label::builder()
                .xalign(0.0)
                .halign(gtk::Align::Start)
                .wrap(true)
                .selectable(true)
                .build();
            device_ids_label.add_css_class("dim-label");
            device_ids_label.add_css_class("caption");

            let device_header = gtk::Box::new(gtk::Orientation::Horizontal, 16);
            device_header.append(&device_icon);
            let device_text = gtk::Box::new(gtk::Orientation::Vertical, 4);
            device_text.set_hexpand(true);
            device_text.set_valign(gtk::Align::Center);
            device_text.append(&device_model_label);
            device_text.append(&device_ids_label);
            device_header.append(&device_text);
            badge_box.set_valign(gtk::Align::Start);
            device_header.append(&badge_box);

            let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
            separator.set_margin_top(16);
            separator.set_margin_bottom(16);

            let node_caption = caption_label();
            let node_label = value_label();
            let channel_caption = caption_label();
            let channel_label = value_label();
            let descriptor_caption = caption_label();
            let descriptor_label = value_label();
            descriptor_label.add_css_class("dim-label");
            let info_grid = gtk::Box::new(gtk::Orientation::Vertical, 12);
            for (caption, value) in [
                (&node_caption, &node_label),
                (&channel_caption, &channel_label),
                (&descriptor_caption, &descriptor_label),
            ] {
                let field = gtk::Box::new(gtk::Orientation::Vertical, 2);
                field.append(caption);
                field.append(value);
                info_grid.append(&field);
            }

            let device_body = gtk::Box::new(gtk::Orientation::Vertical, 0);
            device_body.set_margin_top(20);
            device_body.set_margin_bottom(20);
            device_body.set_margin_start(20);
            device_body.set_margin_end(20);
            device_body.append(&device_header);
            device_body.append(&separator);
            device_body.append(&info_grid);
            let device_card = adw::Bin::new();
            device_card.add_css_class("card");
            device_card.set_child(Some(&device_body));

            // 操作卡：主操作（解锁 / 验证口令）+ 口令管理禁用组 + 证据缺口说明。
            let action_unlock = gtk::Button::new();
            action_unlock.add_css_class("suggested-action");
            let action_validate_password = gtk::Button::new();
            let primary_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            primary_row.set_homogeneous(true);
            primary_row.append(&action_unlock);
            primary_row.append(&action_validate_password);

            let action_set_password = flat_button();
            let action_change_password = flat_button();
            let action_delete_password = flat_button();
            let admin_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            admin_row.set_homogeneous(true);
            admin_row.set_margin_top(12);
            admin_row.append(&action_set_password);
            admin_row.append(&action_change_password);
            admin_row.append(&action_delete_password);

            let password_admin_note = gtk::Label::builder()
                .xalign(0.0)
                .halign(gtk::Align::Start)
                .wrap(true)
                .margin_top(12)
                .build();
            password_admin_note.add_css_class("dim-label");
            password_admin_note.add_css_class("caption");

            let actions_body = gtk::Box::new(gtk::Orientation::Vertical, 0);
            actions_body.set_margin_top(20);
            actions_body.set_margin_bottom(20);
            actions_body.set_margin_start(20);
            actions_body.set_margin_end(20);
            actions_body.append(&primary_row);
            actions_body.append(&admin_row);
            actions_body.append(&password_admin_note);
            let actions_card = adw::Bin::new();
            actions_card.add_css_class("card");
            actions_card.set_child(Some(&actions_body));

            // 进度与结果反馈（无进度 / 无结果时整组不占版面）。
            let progress = gtk::ProgressBar::builder()
                .hexpand(true)
                .valign(gtk::Align::Center)
                .build();
            let action_cancel = gtk::Button::new();
            let progress_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            progress_box.set_visible(false);
            progress_box.append(&progress);
            progress_box.append(&action_cancel);

            let result_icon = gtk::Image::builder()
                .pixel_size(16)
                .valign(gtk::Align::Start)
                .margin_top(2)
                .visible(false)
                .build();
            let result_label = gtk::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .wrap(true)
                .build();
            let result_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            result_row.set_visible(false);
            result_row.append(&result_icon);
            result_row.append(&result_label);

            let feedback_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
            feedback_box.append(&progress_box);
            feedback_box.append(&result_row);

            let dashboard_column = gtk::Box::new(gtk::Orientation::Vertical, 24);
            dashboard_column.set_margin_top(24);
            dashboard_column.set_margin_bottom(24);
            dashboard_column.set_margin_start(24);
            dashboard_column.set_margin_end(24);
            dashboard_column.append(&device_card);
            dashboard_column.append(&actions_card);
            dashboard_column.append(&feedback_box);

            let clamp = adw::Clamp::new();
            clamp.set_maximum_size(600);
            clamp.set_tightening_threshold(500);
            clamp.set_child(Some(&dashboard_column));

            let dashboard_scroller = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&clamp)
                .build();

            // Toast 承载层：成功结果的瞬时提示（libadwaita 特色）。
            let toast_overlay = adw::ToastOverlay::new();
            toast_overlay.set_child(Some(&dashboard_scroller));

            // —— 关于页：版本 / 适用设备 / 协议参考 / 运行环境 ——
            let about_version_row = adw::ActionRow::new();
            let about_device_row = adw::ActionRow::new();
            let about_repository_row = adw::ActionRow::new();
            let about_group = adw::PreferencesGroup::new();
            about_group.add(&about_version_row);
            about_group.add(&about_device_row);
            about_group.add(&about_repository_row);
            let platform_row = adw::ActionRow::new();
            let platform_group = adw::PreferencesGroup::new();
            platform_group.add(&platform_row);
            let about_page = adw::PreferencesPage::new();
            about_page.add(&about_group);
            about_page.add(&platform_group);

            // —— 页面栈（D30：两类页面）——
            let content_stack = gtk::Stack::builder()
                .transition_type(gtk::StackTransitionType::Crossfade)
                .transition_duration(200)
                .hexpand(true)
                .vexpand(true)
                .build();
            content_stack.add_named(&toast_overlay, Some(NavItem::Dashboard.page_name()));
            content_stack.add_named(&about_page, Some(NavItem::About.page_name()));

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
                window_title,
                sidebar_title,
                split_view,
                nav_list,
                nav_labels,
                content_stack,
                toast_overlay,
                device_card,
                device_icon,
                device_model_label,
                device_ids_label,
                status_icon,
                status_label,
                badge_box,
                node_caption,
                node_label,
                channel_caption,
                channel_label,
                descriptor_caption,
                descriptor_label,
                actions_card,
                action_unlock,
                action_validate_password,
                action_set_password,
                action_change_password,
                action_delete_password,
                password_admin_note,
                progress_box,
                progress,
                action_cancel,
                result_row,
                result_icon,
                result_label,
                about_group,
                platform_group,
                about_version_row,
                about_device_row,
                about_repository_row,
                platform_row,
                state: RefCell::new(WindowState::default()),
            }
        }
    }

    /// 弱化说明小标题（内置 caption 类）。
    fn caption_label() -> gtk::Label {
        let label = gtk::Label::builder().xalign(0.0).halign(gtk::Align::Start).build();
        label.add_css_class("dim-label");
        label.add_css_class("caption");
        label
    }

    /// 字段值标签（可选中文本）。
    fn value_label() -> gtk::Label {
        gtk::Label::builder()
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .wrap(true)
            .selectable(true)
            .build()
    }

    /// 次级操作按钮（内置 flat 类）。
    fn flat_button() -> gtk::Button {
        let button = gtk::Button::new();
        button.add_css_class("flat");
        button
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
        window.set_default_size(1024, 680);
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

    /// 文案与初始状态（K5/AC-015：全部经 i18n 键赋值，代码内不内联可显示字符串）。
    fn setup(&self) {
        let imp = self.imp();
        imp.sidebar_title.set_title(&t!("app.title"));
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
        imp.window_title.set_title(&t!(NavItem::Dashboard.label_key()));
        imp.node_caption.set_label(&t!("device.node"));
        imp.channel_caption.set_label(&t!("device.channel_title"));
        imp.descriptor_caption.set_label(&t!("device.descriptor_title"));
        imp.about_group.set_title(&t!(NavItem::About.label_key()));
        imp.about_group
            .set_description(Some(&t!("about.notice")));
        imp.about_version_row.set_title(&t!("about.version_title"));
        imp.about_device_row.set_title(&t!("about.device_title"));
        imp.about_repository_row
            .set_title(&t!("about.repository_title"));
        imp.platform_group
            .set_title(&t!("about.platform_title"));
        imp.about_version_row
            .set_subtitle(env!("CARGO_PKG_VERSION"));
        imp.about_device_row.set_subtitle(t!("app.subtitle").as_ref());
        imp.about_repository_row
            .set_subtitle(t!("about.repository").as_ref());
        for action in ActionId::ALL {
            if let Some(button) = self.action_button(action) {
                button.set_label(&t!(action.label_key()));
            }
        }
        imp.action_cancel.set_label(&t!("action.cancel"));
        imp.action_preferences
            .set_tooltip_text(Some(&t!("action.preferences")));
        imp.sidebar_toggle
            .set_tooltip_text(Some(&t!("nav.toggle_sidebar")));
        // 侧边栏开关常显：桌面宽度也应能收起侧边栏（GNOME 应用惯例）。
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
        imp.action_preferences.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.present_preferences()
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
    /// 同步取数并强制呈现（启动与会话收尾 §3.3）；扫描阶段的失败只记诊断——设备卡空态
    /// 与关于页平台说明已给出路，结果区留给用户触发的作业呈现（与周期轮次同口径）。
    fn refresh_devices(&self) {
        let outcome = jobs::fetch_scan();
        if let Some(error) = &outcome.error {
            diagnostics::ring().record(Level::Warn, presentation::presentation_code(error));
        }
        self.apply_scan(&outcome, true);
    }

    /// 呈现段（主线程）：按重扫状态机裁决并更新设备卡、徽章与入口。
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

    /// 设备卡（§4.12：产品名、VID:PID、设备节点、传输通道、描述符摘要与锁定徽章）。
    fn show_hit(&self, hit: &ScanHit) {
        let imp = self.imp();
        // relocalize 重放缓存：语言切换后按新语言重放设备卡文案。
        self.state().borrow_mut().last_hit = Some(hit.clone());
        imp.device_model_label.set_label(&t!("device.model"));
        let ids = format!(
            "{} {:04x}:{:04x}",
            t!("device.vid_pid"),
            hit.job.vid,
            hit.job.pid
        );
        imp.device_ids_label.set_label(&ids);
        imp.status_label
            .set_label(&t!(hit.job.identity.status_key()));
        self.set_badge_class(hit.job.identity);
        imp.badge_box.set_visible(true);
        let node = match &hit.job.node {
            Some(node) => node.to_string(),
            None => String::new(),
        };
        imp.node_label.set_label(&node);
        imp.channel_label.set_label(t!("device.channel").as_ref());
        imp.descriptor_label
            .set_label(hit.descriptor.as_deref().unwrap_or(""));
        for widget in [
            imp.node_caption.upcast_ref::<gtk::Widget>(),
            imp.node_label.upcast_ref::<gtk::Widget>(),
            imp.channel_caption.upcast_ref::<gtk::Widget>(),
            imp.channel_label.upcast_ref::<gtk::Widget>(),
            imp.descriptor_caption.upcast_ref::<gtk::Widget>(),
            imp.descriptor_label.upcast_ref::<gtk::Widget>(),
        ] {
            widget.set_visible(true);
        }
    }

    /// 设备卡空态（§4.1：不识别为 T7 Shield，入口保持禁用）。
    fn show_unknown_device(&self) {
        let imp = self.imp();
        // 空态与设备卡互斥：清除重放缓存，relocalize 才会重放空态而不是陈旧设备卡。
        self.state().borrow_mut().last_hit = None;
        imp.device_model_label
            .set_label(&t!(DeviceIdentity::Unrecognized.status_key()));
        imp.device_ids_label.set_label(t!("device.empty_hint").as_ref());
        imp.status_label
            .set_label(&t!(DeviceIdentity::Unrecognized.status_key()));
        self.set_badge_class(DeviceIdentity::Unrecognized);
        // 空态卡标题已是「未发现 T7 Shield」，徽章与信息行不再重复呈现。
        imp.badge_box.set_visible(false);
        imp.node_label.set_label("");
        imp.channel_label.set_label("");
        imp.descriptor_label.set_label("");
        for widget in [
            imp.node_caption.upcast_ref::<gtk::Widget>(),
            imp.node_label.upcast_ref::<gtk::Widget>(),
            imp.channel_caption.upcast_ref::<gtk::Widget>(),
            imp.channel_label.upcast_ref::<gtk::Widget>(),
            imp.descriptor_caption.upcast_ref::<gtk::Widget>(),
            imp.descriptor_label.upcast_ref::<gtk::Widget>(),
        ] {
            widget.set_visible(false);
        }
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

    /// 平台说明文案（D27：Linux 通道说明；置于关于页「运行环境」分组）。
    pub fn show_platform_notice(&self) {
        let text = t!("platform.linux_notice").to_string();
        self.imp().platform_row.set_subtitle(text.as_str());
    }

    /// 进度条与步骤文案（§4.13：7 个 `UnlockStep`）。
    pub fn show_step(&self, step: UnlockStep) {
        let imp = self.imp();
        let total = UnlockStep::ALL.len() as f64;
        let done = UnlockStep::ALL
            .iter()
            .position(|candidate| *candidate == step)
            .map(|index| (index + 1) as f64)
            .unwrap_or(0.0);
        // 步骤文案先摘旧结果语义（上一次成功/错误的着色与图标不得残留），
        // 再显示进度区并写步骤文案（不走 show_outcome——那会隐藏进度区）。
        for class in ["success", "error", "dim-label"] {
            imp.result_label.remove_css_class(class);
        }
        imp.result_icon.set_visible(false);
        imp.result_row.set_visible(true);
        imp.progress_box.set_visible(true);
        imp.progress.set_fraction(done / total);
        imp.result_label.set_label(&t!(format!("progress.{step}")));
    }

    /// 结果区统一呈现（§4.12/§6）：文本 + 语义类 + 图标；进度条随结果/错误隐藏；
    /// 成功结果附加 Toast 瞬时提示（libadwaita 特色）。
    pub(crate) fn show_outcome(&self, text: &str, kind: Outcome) {
        let imp = self.imp();
        for class in ["success", "error", "dim-label"] {
            imp.result_label.remove_css_class(class);
            imp.result_icon.remove_css_class(class);
        }
        imp.result_row.set_visible(true);
        imp.result_label.set_label(text);
        match kind {
            Outcome::Neutral => {
                imp.result_label.add_css_class("dim-label");
                imp.result_icon.set_visible(false);
            }
            Outcome::Success => {
                imp.result_label.add_css_class("success");
                imp.result_icon.add_css_class("success");
                imp.result_icon.set_icon_name(Some("emblem-ok-symbolic"));
                imp.result_icon.set_visible(true);
                imp.toast_overlay.add_toast(adw::Toast::new(text));
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

    /// 空闲态：隐藏结果区（无进度 / 无结果 / 无错误时不占版面）。
    pub(crate) fn hide_outcome(&self) {
        let imp = self.imp();
        for class in ["success", "error", "dim-label"] {
            imp.result_label.remove_css_class(class);
            imp.result_icon.remove_css_class(class);
        }
        imp.result_icon.set_visible(false);
        imp.result_row.set_visible(false);
        imp.progress.set_fraction(0.0);
        imp.progress_box.set_visible(false);
    }

    /// 结果文案（§4.8 判据分级 / 取消 / 校验结论）。
    pub fn show_result(&self, message_key: &'static str) {
        // relocalize 重放缓存（success/neutral 均记；重放时重复写同一值，幂等无害）。
        self.state().borrow_mut().last_outcome = Some(StoredOutcome::Result(message_key));
        self.show_outcome(&t!(message_key), result_kind(message_key));
    }

    /// 错误呈现：呈现码 + 一句原因 + 一句建议（§4.13）。
    pub fn show_error(&self, error: &AppError) -> String {
        // relocalize 重放缓存（错误经呈现码/原因/建议键重放）。
        self.state().borrow_mut().last_outcome = Some(StoredOutcome::Error(error.clone()));
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

    /// 切换页面并同步导航选中态与主标题栏页名（选中唯一性由 `GtkListBox` 原生机制保证）。
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
    }

    /// 打开设置对话框（D28/D30）：主题 / 语言 / 工作日志导出；更改即时生效并持久化。
    fn present_preferences(&self) {
        let settings = crate::settings::Settings::load();
        let dialog = crate::ui::settings_dialog::SettingsDialog::new(self, settings);
        dialog.present(Some(self));
    }

    /// 导出脱敏工作日志（D30：入口在设置对话框；§6 导出前再次过滤）。
    pub(crate) fn export_diagnostics(&self) {
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

    /// 入口按钮（按 `ActionId` 取操作卡的按钮）。
    pub fn action_button(&self, action: ActionId) -> Option<gtk::Button> {
        let imp = self.imp();
        Some(match action {
            ActionId::Unlock => imp.action_unlock.clone(),
            ActionId::ValidatePassword => imp.action_validate_password.clone(),
            ActionId::SetPassword => imp.action_set_password.clone(),
            ActionId::ChangePassword => imp.action_change_password.clone(),
            ActionId::DeletePassword => imp.action_delete_password.clone(),
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

    /// 语言切换后重设全部静态文案并重放动态呈现（D28：切换即时生效）。
    pub fn relocalize(&self) {
        let imp = self.imp();
        imp.sidebar_title.set_title(&t!("app.title"));
        for (label, item) in imp.nav_labels.iter().zip(NavItem::ALL) {
            label.set_label(&t!(item.label_key()));
        }
        let current = imp.content_stack.visible_child_name().unwrap_or_default();
        let current_item = NavItem::ALL
            .iter()
            .find(|item| item.page_name() == current.as_str())
            .copied()
            .unwrap_or(NavItem::Dashboard);
        imp.window_title.set_title(&t!(current_item.label_key()));
        imp.node_caption.set_label(&t!("device.node"));
        imp.channel_caption.set_label(&t!("device.channel_title"));
        imp.descriptor_caption.set_label(&t!("device.descriptor_title"));
        imp.about_group.set_title(&t!(NavItem::About.label_key()));
        imp.about_group
            .set_description(Some(&t!("about.notice")));
        imp.about_version_row.set_title(&t!("about.version_title"));
        imp.about_device_row.set_title(&t!("about.device_title"));
        imp.about_repository_row
            .set_title(&t!("about.repository_title"));
        imp.platform_group
            .set_title(&t!("about.platform_title"));
        imp.about_device_row.set_subtitle(t!("app.subtitle").as_ref());
        imp.about_repository_row
            .set_subtitle(t!("about.repository").as_ref());
        for action in ActionId::ALL {
            if let Some(button) = self.action_button(action) {
                button.set_label(&t!(action.label_key()));
            }
        }
        imp.action_cancel.set_label(&t!("action.cancel"));
        imp.action_preferences
            .set_tooltip_text(Some(&t!("action.preferences")));
        imp.sidebar_toggle
            .set_tooltip_text(Some(&t!("nav.toggle_sidebar")));
        // 设备卡重放：有缓存命中按新语言重渲染（不走 refresh_devices——
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
        let dialog = self
            .state()
            .borrow()
            .open_dialog
            .as_ref()
            .and_then(|weak| weak.upgrade());
        if let Some(dialog) = dialog {
            dialog.relocalize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation;

    /// 导航列表行数（`GtkListBox` 无直接计数 API，按索引探测）。
    fn nav_row_count(list: &gtk::ListBox) -> i32 {
        let mut count = 0;
        while list.row_at_index(count).is_some() {
            count += 1;
        }
        count
    }

    /// 锚点（§10）：侧边栏导航 ≥2 项（D30：两类页面）且选中态唯一。
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

        // 结构（D29：代码构建）：导航列表 ≥2 行、挂内置类、页名齐备、选中态唯一。
        if !crate::test_support::gtk_ready("test_sidebar_navigation_items") {
            return;
        }
        let window = MainWindow::new();
        let imp = window.imp();
        let row_count = nav_row_count(&imp.nav_list);
        assert!(row_count >= 2, "导航项至少 2 项（D30 两类页面），实际 {row_count}");
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
        // D30：卡片式仪表盘 + 动态页名标题 + 原生侧边栏品牌标题。
        assert_eq!(imp.device_card.type_().name(), "AdwBin");
        assert_eq!(imp.status_label.type_().name(), "GtkLabel");
        assert_eq!(imp.action_unlock.type_().name(), "GtkButton");
        assert_eq!(imp.progress.type_().name(), "GtkProgressBar");
        assert_eq!(imp.result_label.type_().name(), "GtkLabel");
        assert_eq!(
            imp.toast_overlay.type_().name(),
            "AdwToastOverlay"
        );

        // 启动态：设备未识别 → 全部入口禁用（§4.1）。
        for action in ActionId::ALL {
            let button = window
                .action_button(action)
                .expect("入口按钮必须存在");
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
        assert!(imp.result_label.has_css_class("success"));
        assert!(imp.result_icon.is_visible());
        // 再进入下一次作业的步骤呈现：旧语义必须被摘除。
        window.show_step(UnlockStep::ALL[0]);
        for class in ["success", "error", "dim-label"] {
            assert!(
                !imp.result_label.has_css_class(class),
                "步骤文案不得残留 {class}"
            );
        }
        assert!(!imp.result_icon.is_visible());
        assert!(imp.progress_box.is_visible());
        assert_eq!(
            imp.result_label.label(),
            t!(format!("progress.{}", UnlockStep::ALL[0])).to_string()
        );
    }
}
