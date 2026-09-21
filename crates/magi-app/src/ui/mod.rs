//! UI 子模块（D29）：界面代码的唯一落点，与业务逻辑分层。
//!
//! 组织约定：
//! - 一个窗口 / 对话框一个文件：[`main_window`]（主窗口与三个页面）、[`password_dialog`]、
//!   [`settings_dialog`]；
//! - 业务逻辑（状态机、作业、诊断、呈现码、设置持久化）留在 crate 根模块；UI 只做装配、
//!   呈现与用户交互转发；
//! - 全部界面由 Rust 代码构建（libadwaita 原生组件与标准页面模式），无 `.ui` 模板；
//! - 本模块导出 UI 级胶水：[`apply_theme`]（D28 主题的运行时应用）。

use libadwaita as adw;

pub mod main_window;
pub mod password_dialog;
pub mod settings_dialog;
pub mod environment_dialog;
pub mod environment_flow;

pub use main_window::MainWindow;
pub use password_dialog::PasswordDialog;
pub use settings_dialog::SettingsDialog;
pub use environment_dialog::EnvironmentDialog;

use crate::settings::Settings;

/// 应用主题到全局样式管理器（D28）。
///
/// 须在 GTK 初始化之后调用（`AdwApplication` 的 `startup` 阶段及以后），否则
/// `StyleManager::default()` 会触发「Gtk has to be initialized」断言。
pub fn apply_theme(settings: &Settings) {
    adw::StyleManager::default().set_color_scheme(settings.color_scheme());
}
