//! §4.12 口令对话框（D29：界面由 Rust 代码构建，使用 libadwaita 原生组件）。
//!
//! 口令纪律（§6）：口令经 [`magi_protocol::Password`]（`Zeroizing<Vec<u8>>`）承载，不进 `String`
//! 持有的长生命周期结构；提交后立即取走并清空输入框副本；报文构造完成后由协议层 `zeroize`，
//! 结构体 `Drop` 再清零一次；不写日志（含长度）、不进剪贴板、不落盘。
//!
//! 零回显（§4.12/§6）：`GtkPasswordEntry` 的类型固有行为即输入恒被遮蔽，代码不设置任何可见性
//! 属性，并关闭「显示明文」图标（`show-peek-icon = false`），对话框中不存在任何回显通路。

use std::cell::Cell;

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use magi_protocol::Password;
use rust_i18n::t;

use crate::device::gate::{self, ActionId};
use crate::presentation::AppError;

mod imp {
    use super::*;

    /// 口令对话框子件（D29：全部在 Rust 侧构建）。
    pub struct PasswordDialog {
        pub toolbar: adw::ToolbarView,
        pub dialog_title: adw::WindowTitle,
        pub body_label: gtk::Label,
        pub entry: gtk::PasswordEntry,
        pub error_label: gtk::Label,
        pub submit: gtk::Button,
        pub cancel: gtk::Button,
        pub action: Cell<ActionId>,
    }

    impl Default for PasswordDialog {
        fn default() -> Self {
            // 文案装配在 `PasswordDialog::new(action)` 中完成（本函数只负责结构与默认态）。
            let dialog_title = adw::WindowTitle::new("", "");
            let header = adw::HeaderBar::new();
            header.set_title_widget(Some(&dialog_title));

            let icon = gtk::Image::from_icon_name("dialog-password-symbolic");
            icon.set_pixel_size(32);
            icon.set_valign(gtk::Align::Start);
            icon.add_css_class("dim-label");

            let body_label = gtk::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .wrap(true)
                .build();
            body_label.add_css_class("dim-label");
            let body_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            body_box.append(&icon);
            body_box.append(&body_label);

            let entry = gtk::PasswordEntry::builder()
                .show_peek_icon(false)
                .activates_default(true)
                .hexpand(true)
                .build();

            let error_label = gtk::Label::builder().xalign(0.0).wrap(true).build();

            let submit = gtk::Button::new();
            submit.add_css_class("suggested-action");
            let cancel = gtk::Button::new();
            let button_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            button_box.set_halign(gtk::Align::End);
            button_box.append(&cancel);
            button_box.append(&submit);

            let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
            content.set_margin_top(18);
            content.set_margin_bottom(18);
            content.set_margin_start(18);
            content.set_margin_end(18);
            content.append(&body_box);
            content.append(&entry);
            content.append(&error_label);
            content.append(&button_box);

            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&header);
            toolbar.set_content(Some(&content));

            Self {
                toolbar,
                dialog_title,
                body_label,
                entry,
                error_label,
                submit,
                cancel,
                action: Cell::new(ActionId::Unlock),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PasswordDialog {
        const NAME: &'static str = "T7PasswordDialog";
        type Type = super::PasswordDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for PasswordDialog {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_content_width(400);
            obj.set_follows_content_size(true);
            obj.set_child(Some(&self.toolbar));
        }
    }

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
        dialog.imp().action.set(action);
        dialog.relocalize();
        dialog.imp().error_label.set_label("");
        dialog
    }

    /// 主按钮（返回输入框焦点前的最终动作由调用方接线）。
    pub fn submit_button(&self) -> gtk::Button {
        self.imp().submit.clone()
    }

    /// 取消按钮。
    pub fn cancel_button(&self) -> gtk::Button {
        self.imp().cancel.clone()
    }

    /// 语言切换后重设对话框文案（D28）：标题/说明/按钮/占位符全部重取。
    pub fn relocalize(&self) {
        let imp = self.imp();
        imp.dialog_title.set_title(&t!(imp.action.get().label_key()));
        imp.body_label.set_label(&t!("password.body"));
        imp.submit.set_label(&t!("password.submit"));
        imp.cancel.set_label(&t!("password.cancel"));
        imp.entry
            .set_placeholder_text(Some(&t!("password.placeholder")));
    }

    /// §4.12：取走口令输入 —— 空/全空白 → [`AppError::EmptyPassword`]（就地提示，保持打开、
    /// 不构造任何报文）；成功时立即清空输入框内的副本并返回零化缓冲。
    pub fn take_password(&self) -> Result<Password, AppError> {
        let text = self.imp().entry.text();
        let password = gate::validate_password_input(text.as_str())?;
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
        let error_label = &self.imp().error_label;
        error_label.set_label(&text);
        error_label.add_css_class("error");
        self.imp().entry.grab_focus();
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        response_frame, start_session_body_rejected, FakeTransport, START_SESSION_BODY_ACCEPTED,
    };
    use magi_protocol::run_validate_password;

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

        let mut password = gate::validate_password_input(SECRET).expect("非空口令");
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

        let mut password = gate::validate_password_input(SECRET).expect("非空口令");
        let outcome =
            run_validate_password(&transport, comid, &mut password).expect("收发必须成功");
        assert!(!outcome.accepted);
        assert_eq!(outcome.status_byte, 1);
        assert!(password.expose().is_empty(), "被拒路径同样必须清零");
        assert_eq!(
            AppError::from(magi_protocol::RunError::Protocol(
                magi_protocol::ProtocolError::SessionRejected { status_byte: 1 }
            )),
            AppError::PasswordRejected
        );
    }

    /// 对话框装配（K6）：`gtk::init()` 失败时打印跳过原因并返回，不 `#[ignore]`。
    #[test]
    fn test_password_dialog_instantiates() {
        if !crate::test_support::gtk_ready("test_password_dialog_instantiates") {
            return;
        }
        let dialog = PasswordDialog::new(ActionId::ValidatePassword);
        let imp = dialog.imp();
        assert_eq!(imp.entry.type_().name(), "GtkPasswordEntry");
        assert_eq!(imp.submit.type_().name(), "GtkButton");
        // 输入不回显（§4.12）：`GtkPasswordEntry` 没有 `visibility` 属性、输入恒被遮蔽；
        // 本对话框还要关闭「显示明文」图标，使界面里不存在回显通路（§6）。
        assert!(!imp.entry.property::<bool>("show-peek-icon"));
        assert!(imp.entry.property::<bool>("activates-default"));

        // 空口令提交：就地提示、不产生口令缓冲。
        imp.entry.set_text("");
        assert_eq!(
            dialog.take_password().expect_err("空口令必须被拒绝"),
            AppError::EmptyPassword
        );
        assert_eq!(
            imp.error_label.label(),
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
        assert_eq!(imp.entry.text(), "");
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
