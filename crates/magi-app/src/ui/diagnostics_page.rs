//! 诊断页（D29 分文件）：只读记录卡片与脱敏导出入口的构建与刷新。

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use crate::diagnostics;
use crate::ui::MainWindow;

/// 诊断页控件集（构建于本文件，装配进主窗口 imp）。
pub(crate) struct DiagnosticsWidgets {
    pub diagnostics_group: adw::PreferencesGroup,
    pub diagnostics_view: gtk::TextView,
    pub action_export_diagnostics: adw::ButtonRow,
}

/// 构建诊断页（返回页面根控件与控件集）。
pub(crate) fn build() -> (gtk::Widget, DiagnosticsWidgets) {
    // 诊断页：只读记录卡片 + 脱敏导出入口。
    let diagnostics_view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(12)
        .bottom_margin(12)
        .left_margin(12)
        .right_margin(12)
        .build();
    let diagnostics_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(240)
        .child(&diagnostics_view)
        .build();
    diagnostics_scroller.add_css_class("card");
    let action_export_diagnostics = adw::ButtonRow::new();
    let diagnostics_group = adw::PreferencesGroup::new();
    diagnostics_group.add(&diagnostics_scroller);
    diagnostics_group.add(&action_export_diagnostics);
    let diagnostics_page = adw::PreferencesPage::new();
    diagnostics_page.add(&diagnostics_group);
    (
        diagnostics_page.upcast(),
        DiagnosticsWidgets {
            diagnostics_group,
            diagnostics_view,
            action_export_diagnostics,
        },
    )
}

impl MainWindow {
    /// 诊断页内容：环形缓冲的脱敏导出文本（只读、不可编辑）。
    pub(crate) fn refresh_diagnostics_view(&self) {
        let text = diagnostics::ring().export_redacted();
        let buffer = self.imp().diagnostics.diagnostics_view.buffer();
        buffer.set_text(&text);
    }
}
