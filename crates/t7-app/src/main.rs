//! t7-app 入口：`adw::Application` 启动、locale 初始化、空口令/入口装配。
//!
//! W01 只提供最小可编译占位（`adw::Application` 初始化，不接任何协议行为）；locale
//! 初始化与入口装配由后续任务补齐。
//!
//! 约束：UI 主线程只做界面与编排，所有协议 I/O 在工作线程；口令不落盘、不入日志；窗口内
//! 可显示文案一律经 i18n 键渲染，代码中不内联可显示字符串。

mod controller;
mod diagnostics;
mod jobs;
mod main_window;
mod observation;
mod password_dialog;
mod presentation;

use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

/// 应用 ID（非用户可见文案；W01 占位值）。
const APP_ID: &str = "dev.rikki.T7Magician";

fn main() -> gtk::glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.run()
}

#[cfg(test)]
mod tests {
    /// W01 冒烟测试：确认 crate 可编译且元数据可见（脚手架验收，非行为测试）。
    #[test]
    fn test_crate_builds() {
        assert_eq!(env!("CARGO_PKG_NAME"), "t7-app");
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
    }
}
