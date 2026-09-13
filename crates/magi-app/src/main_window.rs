//! §4.12 主窗口：设备卡片、锁定状态、操作入口、进度与结果反馈。
//!
//! 结构全部来自 `ui/main_window.ui`（K5：`.ui` 只放 id/class，文案零字面量），本文件只做
//! 文案赋值、入口敏感度刷新与流程装配；协议 I/O 一律经 [`crate::jobs`] 在工作线程执行。
//!
//! 主流程（§4.13）：启动即扫描设备 → 更新设备卡片/状态/入口 → 用户触发 → 口令对话框 →
//! 工作线程作业 → `AppEvent` 回主线程更新进度与结果；取消只停止后续步骤，不阻塞退出（§6）。

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

use crate::controller::{
    self, ActionId, DeviceIdentity, PlatformCapability, PlatformNotice, UnlockGate,
    REASON_PLATFORM_UNAVAILABLE,
};
use crate::diagnostics::{self, Level};
use crate::jobs::{self, CancelFlag, DeviceJob, ScanHit};
use crate::password_dialog::PasswordDialog;
use crate::presentation::{self, AppError, AppEvent};

/// 窗口运行期状态（设备、入口闸门、取消标志与在飞动作）。
#[derive(Debug, Default)]
pub(crate) struct WindowState {
    gate: UnlockGate,
    device: Option<DeviceJob>,
    cancel: CancelFlag,
    action: Option<ActionId>,
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
        pub platform_notice_label: TemplateChild<gtk::Label>,
        pub(crate) state: RefCell<WindowState>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MainWindow {
        const NAME: &'static str = "T7MainWindow";
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

    /// 启动装配（§4.13）：注册事件汇、接线入口、扫描设备。
    pub fn start(&self) {
        let this = self.clone();
        jobs::set_event_sink(move |event| this.on_event(event));
        self.connect_actions();
        self.refresh_devices();
        self.refresh_actions();
    }

    /// 文案与初始状态：`.ui` 内无字面量，全部显示文本在此经 i18n 键赋值。
    fn setup(&self) {
        let imp = self.imp();
        imp.window_title.set_title(&t!("app.title"));
        imp.window_title.set_subtitle(&t!("app.subtitle"));
        imp.device_group_title.set_label(&t!("device.group_title"));
        imp.device_model_label.set_label(&t!("device.model"));
        for action in ActionId::ALL {
            if let Some(button) = self.action_button(action) {
                button.set_label(&t!(action.label_key()));
            }
        }
        imp.action_cancel.set_label(&t!("action.cancel"));
        imp.action_export_diagnostics
            .set_label(&t!("action.export_diagnostics"));
        imp.progress.set_fraction(0.0);
        imp.result_label.set_label(&t!("progress.idle"));
        self.show_unknown_device();
        self.apply_actions(
            PlatformCapability::current(),
            DeviceIdentity::Unrecognized,
            &UnlockGate::new(),
        );
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
    }

    /// 扫描设备并更新设备卡片（§4.1/§4.5：Linux sysfs / macOS 只读描述符侦察）。
    fn refresh_devices(&self) {
        let capability = PlatformCapability::current();
        self.show_platform_notice(&controller::platform_notice(capability));

        let hits = match jobs::scan_devices() {
            Ok(hits) => hits,
            Err(error) => {
                self.show_error(&error);
                Vec::new()
            }
        };
        let (hits, limit_key) = controller::clamp_devices(hits);
        if let Some(key) = limit_key {
            // §6：同时受理设备上限 8 个，超出部分不呈现并给出提示。
            diagnostics::ring().record(Level::Warn, key);
        }

        let mut state = self.state().borrow_mut();
        match hits.first() {
            Some(hit) => {
                state.device = Some(hit.job.clone());
                drop(state);
                self.show_hit(hit);
            }
            None => {
                state.device = None;
                drop(state);
                self.show_unknown_device();
            }
        }
        self.refresh_actions();
    }

    /// 按平台能力 + 设备态 + 重试预算刷新入口敏感度与禁用理由（§4.12/§4.11/§4.5）。
    pub fn apply_actions(
        &self,
        capability: PlatformCapability,
        identity: DeviceIdentity,
        gate: &UnlockGate,
    ) {
        let notice = controller::platform_notice(capability);
        for action in ActionId::ALL {
            let Some(button) = self.action_button(action) else {
                continue;
            };
            let platform_allows = notice
                .actions
                .iter()
                .any(|(candidate, enabled)| *candidate == action && *enabled);
            let enabled = platform_allows && gate.allows(action, identity.state());
            let reason_key = if platform_allows {
                gate.disabled_reason_key(action, identity.state())
            } else {
                Some(REASON_PLATFORM_UNAVAILABLE)
            };
            button.set_sensitive(enabled);
            button.set_tooltip_text(reason_key.map(|key| t!(key).to_string()).as_deref());
        }
    }

    /// 按当前状态刷新入口（平台能力 + 设备态 + 重试预算的三个来源都在此汇合）。
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
        self.apply_actions(PlatformCapability::current(), identity, &gate);
    }

    /// 设备卡片（§4.12：型号、VID:PID、设备节点或平台通道、锁定状态、描述符摘要）。
    fn show_hit(&self, hit: &ScanHit) {
        let imp = self.imp();
        imp.status_label
            .set_label(&t!(hit.job.identity.status_key()));
        imp.device_model_label.set_label(&t!("device.model"));
        imp.device_ids_label.set_label(&format!(
            "{} {:04x}:{:04x}",
            t!("device.vid_pid"),
            hit.job.vid,
            hit.job.pid
        ));
        imp.device_node_label.set_label(&match &hit.job.node {
            Some(node) => format!("{} {node}", t!("device.node")),
            None => t!("device.node").to_string(),
        });
        let channel_key = match PlatformCapability::current() {
            PlatformCapability::ScsiAvailable => "device.channel_linux",
            PlatformCapability::MacOsDescriptorOnly => "device.channel_macos",
        };
        imp.device_channel_label.set_label(&t!(channel_key));
        imp.device_descriptor_label
            .set_label(hit.descriptor.as_deref().unwrap_or(""));
    }

    /// 设备卡片空态（§4.1：不识别为 T7 Shield，入口保持禁用）。
    fn show_unknown_device(&self) {
        let imp = self.imp();
        imp.status_label
            .set_label(&t!(DeviceIdentity::Unrecognized.status_key()));
        imp.device_model_label.set_label(&t!("device.model"));
        imp.device_ids_label.set_label("");
        imp.device_node_label.set_label("");
        imp.device_channel_label.set_label("");
        imp.device_descriptor_label.set_label("");
    }

    /// 平台限制文案（§4.5：macOS 呈现已证实限制与 `issues/` 指针）。
    pub fn show_platform_notice(&self, notice: &PlatformNotice) {
        // 两份平台文案按同一模板渲染；`issue` 占位只在 macOS 文案里出现（§4.5）。
        let text = t!(notice.message_key, issue = t!("issue.macos_transport")).to_string();
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
        self.imp().progress.set_fraction(done / total);
        self.imp()
            .result_label
            .set_label(&t!(format!("progress.{step}")));
    }

    /// 结果文案（§4.8 判据分级 / 取消 / 校验结论）。
    pub fn show_result(&self, message_key: &'static str) {
        self.imp().result_label.set_label(&t!(message_key));
        self.imp().progress.set_fraction(0.0);
    }

    /// 错误呈现：呈现码 + 一句原因 + 一句建议（§4.13）。
    pub fn show_error(&self, error: &AppError) -> String {
        let code = presentation::presentation_code(error);
        let text = format!(
            "{code}：{}；{}",
            t!(presentation::reason_key(error)),
            t!(presentation::advice_key(error)),
        );
        self.imp().result_label.set_label(&text);
        self.imp().progress.set_fraction(0.0);
        diagnostics::ring().record(Level::Error, code);
        text
    }

    /// 窗口运行期状态（设备、闸门、取消标志与在飞动作）。
    fn state(&self) -> &RefCell<WindowState> {
        &self.imp().state
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
                self.imp().result_label.set_label(&text);
                self.imp().progress.set_fraction(0.0);
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

        // 锁定态 + 有可用通道：解锁/校验可用，写口令入口仍禁用（§4.11）。
        window.apply_actions(
            PlatformCapability::ScsiAvailable,
            DeviceIdentity::Locked,
            &UnlockGate::new(),
        );
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

        // macOS 只读侦察：即使设备是锁定态，入口也全部禁用（§4.5）。
        window.apply_actions(
            PlatformCapability::MacOsDescriptorOnly,
            DeviceIdentity::Locked,
            &UnlockGate::new(),
        );
        for action in ActionId::ALL {
            assert!(
                !window.action_button(action).unwrap().is_sensitive(),
                "{action:?} 在只读侦察平台上必须禁用"
            );
        }

        // 错误呈现：呈现码 + 原因 + 建议（§4.13）。
        let text = window.show_error(&presentation::AppError::EmptyPassword);
        assert!(text.starts_with("EmptyPassword"));
        assert!(text.contains(&t!("EmptyPassword.reason").to_string()));
        assert!(text.contains(&t!("EmptyPassword.advice").to_string()));
    }
}
