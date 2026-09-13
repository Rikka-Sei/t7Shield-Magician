//! §4.12 口令对话框（`CompositeTemplate`）：输入不回显、提交校验与口令生命周期。
//!
//! 口令纪律（§6）：口令经 [`t7_protocol::Password`]（`Zeroizing<Vec<u8>>`）承载，不进 `String`
//! 持有的长生命周期结构；提交后立即取走并清空输入框副本；报文构造完成后由协议层 `zeroize`，
//! 结构体 `Drop` 再清零一次；不写日志（含长度）、不进剪贴板、不落盘。

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::CompositeTemplate;
use gtk4 as gtk;
use libadwaita as adw;
use rust_i18n::t;
use t7_protocol::Password;

use crate::controller::{self, ActionId};
use crate::presentation::AppError;

mod imp {
    use super::*;

    #[derive(CompositeTemplate, Default)]
    #[template(file = "ui/password_dialog.ui")]
    pub struct PasswordDialog {
        #[template_child]
        pub entry: TemplateChild<gtk::PasswordEntry>,
        #[template_child]
        pub submit: TemplateChild<gtk::Button>,
        #[template_child]
        pub cancel: TemplateChild<gtk::Button>,
        #[template_child]
        pub dialog_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub body_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub error_label: TemplateChild<gtk::Label>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PasswordDialog {
        const NAME: &'static str = "T7PasswordDialog";
        type Type = super::PasswordDialog;
        type ParentType = adw::Dialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }
    }

    impl ObjectImpl for PasswordDialog {}
    impl WidgetImpl for PasswordDialog {}
    impl adw::subclass::prelude::AdwDialogImpl for PasswordDialog {}
}

glib::wrapper! {
    /// 口令对话框（§4.12）。
    pub struct PasswordDialog(ObjectSubclass<imp::PasswordDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

impl Default for PasswordDialog {
    fn default() -> Self {
        Self::new(ActionId::Unlock)
    }
}

impl PasswordDialog {
    /// 构造对话框：`action` 决定标题与说明文案（解锁 / 校验口令共用同一对话框）。
    pub fn new(action: ActionId) -> Self {
        let dialog: Self = glib::Object::new();
        let imp = dialog.imp();
        imp.dialog_title.set_title(&t!(action.label_key()));
        imp.body_label.set_label(&t!("password.body"));
        imp.error_label.set_label("");
        imp.submit.set_label(&t!("password.submit"));
        imp.cancel.set_label(&t!("password.cancel"));
        imp.entry
            .set_placeholder_text(Some(&t!("password.placeholder")));
        dialog
    }

    /// 主按钮（返回输入框焦点前的最终动作由调用方接线）。
    pub fn submit_button(&self) -> gtk::Button {
        self.imp().submit.get()
    }

    /// 取消按钮。
    pub fn cancel_button(&self) -> gtk::Button {
        self.imp().cancel.get()
    }

    /// §4.12：取走口令输入 —— 空/全空白 → [`AppError::EmptyPassword`]（就地提示，保持打开、
    /// 不构造任何报文）；成功时立即清空输入框内的副本并返回零化缓冲。
    pub fn take_password(&self) -> Result<Password, AppError> {
        let text = self.imp().entry.text();
        let password = controller::validate_password_input(text.as_str())?;
        // 提交后不再保留 UI 侧的口令副本（GTK 内部的条目缓冲立即被空串覆盖）。
        self.imp().entry.set_text("");
        self.imp().error_label.set_label("");
        Ok(password)
    }

    /// 就地提示错误（空口令 / 口令被拒），并把焦点交回输入框。
    pub fn show_error(&self, error: &AppError) {
        let text = format!(
            "{}：{}",
            crate::presentation::presentation_code(error),
            t!(crate::presentation::reason_key(error)),
        );
        self.imp().error_label.set_label(&text);
        self.imp().entry.grab_focus();
    }

    /// 清空输入框（重新打开对话框时调用：不残留上一次输入）。
    pub fn reset(&self) {
        self.imp().entry.set_text("");
        self.imp().error_label.set_label("");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        response_frame, start_session_body_rejected, FakeTransport, START_SESSION_BODY_ACCEPTED,
    };
    use t7_protocol::run_validate_password;

    /// 锚点（§10）：提交后口令缓冲被 zeroize。
    ///
    /// 走真实提交路径：`validate_password_input` → 协议层 `run_validate_password`（报文构造
    /// 完成后立即 `zeroize`）。断言清零发生在 drop **之前**（缓冲仍被 `Password` 持有）。
    #[test]
    fn test_password_zeroized_after_submit() {
        const SECRET: &str = "hunter2-secret";
        let comid: u16 = 0x1004; // 测试局部变量（D03：不得写成生产常量）
        let transport = FakeTransport::with_responses(vec![
            response_frame(comid, &START_SESSION_BODY_ACCEPTED),
            // EndSession 应答：`FA` 单字节（§4.7 收尾命令 `data_len = 1`）。
            response_frame(comid, &[0xfa]),
        ]);

        let mut password = controller::validate_password_input(SECRET).expect("非空口令");
        let buffer = password.expose();
        let pointer = buffer.as_ptr();
        let length = buffer.len();
        assert_eq!(length, SECRET.len());

        let outcome = run_validate_password(&transport, comid, &mut password)
            .expect("口令校验的收发必须成功");
        assert!(outcome.accepted);
        assert!(password.expose().is_empty(), "提交后口令缓冲必须已清空");

        // 缓冲区内容确实被清零：zeroize 1.9 的 `Vec` 实现清零整段容量且不释放（缓冲仍存活）。
        let bytes = unsafe { std::slice::from_raw_parts(pointer, length) };
        assert!(
            bytes.iter().all(|byte| *byte == 0),
            "口令缓冲未被清零：{bytes:?}"
        );

        // §4.9/AC-008：口令校验只发 StartSession 与 EndSession 两条 OUT，不发 StartTransaction。
        assert_eq!(transport.outbound_count(), 2);

        // 诊断侧：只记结构字段（CDB 与响应长度判据），不记请求载荷及其长度（载荷长度即口令长度）。
        let diagnostics = crate::diagnostics::DiagnosticsRing::new();
        for exchange in transport.exchanges() {
            diagnostics.record_exchange(&exchange.cdb, exchange.direction, None);
        }
        let export = diagnostics.export_redacted();
        assert!(!export.contains(SECRET), "导出不得含口令原文");
        assert!(
            !export.contains(&hex(SECRET.as_bytes())),
            "导出不得含口令的十六进制形态"
        );
        assert!(!export.contains(&format!("response_len={length}")));
        assert!(!export.contains(&format!("payload_len={length}")));

        // 纵深防御：即使某条记录混入了字节形态，导出前仍会被再次过滤（§6）。
        diagnostics.record(
            crate::diagnostics::Level::Warn,
            &format!("leaked={}", hex(SECRET.as_bytes())),
        );
        let export = diagnostics.export_redacted();
        assert!(!export.contains(&hex(SECRET.as_bytes())));
        assert!(!export.contains(SECRET));

        // 口令缓冲清零后结构体自身也不再保留可读副本。
        drop(password);
    }

    /// 口令被拒的提交路径同样先清零，错误分类为 `PasswordRejected`（§5）。
    #[test]
    fn test_rejected_password_zeroizes_and_maps_to_rejected() {
        const SECRET: &str = "wrong-secret";
        let comid: u16 = 0x1004; // 测试局部变量（D03）
        let transport = FakeTransport::with_responses(vec![response_frame(
            comid,
            &start_session_body_rejected(),
        )]);

        let mut password = controller::validate_password_input(SECRET).expect("非空口令");
        let outcome =
            run_validate_password(&transport, comid, &mut password).expect("收发必须成功");
        assert!(!outcome.accepted);
        assert_eq!(outcome.status_byte, 1);
        assert!(password.expose().is_empty(), "被拒路径同样必须清零");
        assert_eq!(
            AppError::from(t7_protocol::RunError::Protocol(
                t7_protocol::ProtocolError::SessionRejected { status_byte: 1 }
            )),
            AppError::PasswordRejected
        );
    }

    /// 模板装载（K6）：`gtk::init()` 失败时打印跳过原因并返回，不 `#[ignore]`。
    #[test]
    fn test_password_dialog_instantiates_with_template() {
        if !crate::test_support::gtk_ready("test_password_dialog_instantiates_with_template") {
            return;
        }
        let dialog = PasswordDialog::new(ActionId::ValidatePassword);
        let imp = dialog.imp();
        assert_eq!(imp.entry.get().type_().name(), "GtkPasswordEntry");
        assert_eq!(imp.submit.get().type_().name(), "GtkButton");
        // 输入不回显（§4.12），且不提供回显切换图标（§6）。
        assert!(!imp.entry.get().property::<bool>("visibility"));
        assert!(!imp.entry.get().property::<bool>("show-peek-icon"));
        assert!(imp.entry.get().property::<bool>("activates-default"));

        // 空口令提交：就地提示、不产生口令缓冲。
        imp.entry.set_text("");
        assert_eq!(
            dialog.take_password().expect_err("空口令必须被拒绝"),
            AppError::EmptyPassword
        );
        assert_eq!(
            imp.error_label.get().label(),
            format!(
                "{}：{}",
                crate::presentation::CODE_EMPTY_PASSWORD,
                t!(crate::presentation::reason_key(&AppError::EmptyPassword))
            )
        );

        // 非空口令：取走后输入框立即清空（不留 UI 侧副本）。
        imp.entry.set_text("hunter2");
        let password = dialog.take_password().expect("非空口令必须被接受");
        assert_eq!(password.expose(), b"hunter2");
        assert_eq!(imp.entry.get().text(), "");
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
