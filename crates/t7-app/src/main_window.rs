//! §4.12 主窗口：设备卡片、锁定状态、操作入口、进度与结果反馈。
//!
//! 结构全部来自 `ui/main_window.ui`（K5：`.ui` 只放 id/class，文案零字面量），本文件只做
//! 文案赋值、入口敏感度刷新与流程装配；协议 I/O 一律经 [`crate::jobs`] 在工作线程执行。

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::CompositeTemplate;
use gtk4 as gtk;
use libadwaita as adw;
use rust_i18n::t;

use crate::controller::{self, ActionId, DeviceIdentity, UnlockGate};

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
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MainWindow {
        const NAME: &'static str = "T7MainWindow";
        type Type = super::MainWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
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

    /// 把主窗口挂到应用上（激活时呈现一次）。
    pub fn install(app: &adw::Application) {
        app.connect_activate(|app| {
            let window = MainWindow::new();
            window.set_application(Some(app));
            window.present();
        });
    }

    /// 文案与初始状态：`.ui` 内无字面量，全部显示文本在此经 i18n 键赋值。
    fn setup(&self) {
        let imp = self.imp();
        imp.window_title.set_title(&t!("app.title"));
        imp.window_title.set_subtitle(&t!("app.subtitle"));
        imp.device_group_title.set_label(&t!("device.group_title"));
        imp.device_model_label.set_label(&t!("device.model"));
        imp.device_ids_label.set_label("");
        imp.device_node_label.set_label("");
        imp.device_channel_label.set_label("");
        imp.device_descriptor_label.set_label("");
        imp.status_label
            .set_label(&t!(DeviceIdentity::Unrecognized.status_key()));
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
        imp.platform_notice_label
            .set_label(&t!("platform.linux_notice"));

        // 启动态：设备尚未识别，全部入口禁用（§4.1/§4.12）。
        self.apply_actions(DeviceIdentity::Unrecognized, &UnlockGate::new());
    }

    /// 按启用矩阵刷新入口敏感度与禁用理由提示（§4.12/§4.11）。
    pub fn apply_actions(&self, identity: DeviceIdentity, gate: &UnlockGate) {
        for action in ActionId::ALL {
            let Some(button) = self.action_button(action) else {
                continue;
            };
            button.set_sensitive(gate.allows(action, identity.state()));
            button.set_tooltip_text(
                gate.disabled_reason_key(action, identity.state())
                    .map(|key| t!(key).to_string())
                    .as_deref(),
            );
        }
    }

    /// 设备卡片与状态文案（§4.12：型号、VID:PID、设备节点或平台通道、锁定状态）。
    pub fn show_device(
        &self,
        identity: DeviceIdentity,
        vid: u16,
        pid: u16,
        node: Option<&str>,
        channel_key: &'static str,
    ) {
        let imp = self.imp();
        imp.status_label.set_label(&t!(identity.status_key()));
        imp.device_ids_label
            .set_label(&format!("{} {vid:04x}:{pid:04x}", t!("device.vid_pid")));
        imp.device_node_label.set_label(
            &node
                .map(|node| format!("{} {node}", t!("device.node")))
                .unwrap_or_else(|| t!("device.node").to_string()),
        );
        imp.device_channel_label.set_label(&t!(channel_key));
        imp.device_model_label.set_label(&t!("device.model"));
    }

    /// 设备卡片上的描述符侦察摘要（仅 macOS 只读侦察有内容）。
    pub fn show_descriptor_summary(&self, summary: &str) {
        self.imp().device_descriptor_label.set_label(summary);
    }

    /// 平台限制文案（§4.5：macOS 呈现已证实限制与 `issues/` 指针）。
    pub fn show_platform_notice(&self, notice: &controller::PlatformNotice) {
        // 两份平台文案都按同一个模板渲染；`issue` 占位只在 macOS 文案里出现（§4.5）。
        let text = t!(notice.message_key, issue = t!("issue.macos_transport")).to_string();
        self.imp().platform_notice_label.set_label(&text);
    }

    /// 进度条与步骤文案（§4.13：7 个 `UnlockStep`）。
    pub fn show_progress(&self, fraction: f64, step_key: &'static str) {
        self.imp().progress.set_fraction(fraction);
        self.imp().result_label.set_label(&t!(step_key));
    }

    /// 结果文案（§4.8 判据分级 / 取消 / 校验结论）。
    pub fn show_result(&self, message_key: &'static str) {
        self.imp().result_label.set_label(&t!(message_key));
        self.imp().progress.set_fraction(0.0);
    }

    /// 错误呈现：呈现码 + 一句原因 + 一句建议（§4.13）。
    pub fn show_error(&self, error: &crate::presentation::AppError) -> String {
        let code = crate::presentation::presentation_code(error);
        let text = format!(
            "{code}：{}；{}",
            t!(crate::presentation::reason_key(error)),
            t!(crate::presentation::advice_key(error)),
        );
        self.imp().result_label.set_label(&text);
        self.imp().progress.set_fraction(0.0);
        text
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
        // §4.12：口令输入不回显，且不提供回显切换图标（§6）。
        assert_eq!(
            property_value(dialog, "visibility"),
            Some("false".to_string())
        );
        assert_eq!(
            property_value(dialog, "show-peek-icon"),
            Some("false".to_string())
        );
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

        // 锁定态 + 空预算：解锁/校验可用，写口令入口仍禁用（§4.11）。
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
    }
}
