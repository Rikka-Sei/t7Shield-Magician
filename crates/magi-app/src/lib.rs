//! magi-app：MagiShield（T7 Shield GUI 客户端）的界面与线程编排（GTK4 + libadwaita）。
//!
//! 依赖方向（§3.1）：`magi-app` → `magi-protocol` → `magi-transport`；本 crate 不构造 CDB 与令牌流。
//!
//! 约束（§4.12/§4.13、§6）：UI 主线程只做界面与编排，所有协议 I/O 在工作线程；口令不落盘、
//! 不入日志、不进剪贴板；窗口内可显示文案一律经 i18n 键渲染，代码中不内联可显示字符串
//! （AC-015）。
//!
//! 形态说明：本 crate 是库 + 极薄可执行入口（`src/main.rs`）。界面与编排放在库里，才能让
//! 入口启用矩阵、单飞约束、口令清零等口径在无 display 环境下被 `cargo test -p magi-app` 覆盖
//! （K6）；`main.rs` 只负责把控制权交给 [`run`]。
//!
//! UI 分层（D29）：界面代码集中在 [`ui`] 子模块（一个窗口 / 对话框一个文件，全部由 Rust 代码
//! 构建 libadwaita 原生组件）；状态机与作业等设备域逻辑收进 [`device`]，诊断、呈现码与设置持久化留在 crate 根模块。

// §6 国际化：编译期内嵌 locales/*.yml，默认 zh-CN、提供 en。
rust_i18n::i18n!("locales", fallback = "zh-CN");

/// 内存环形缓冲（512 条）+ 脱敏导出（§6「合规与保留」）。
pub mod diagnostics;

/// 设备域模块（D34）：环境就绪、入口闸门、重扫呈现与作业编排等单一关注点模块，收进域目录。
pub mod device;

/// `AppError` → 呈现码映射与文案键（§4.13 表）。
pub mod presentation;

/// D28 应用内设置：主题/语言三态、KeyFile 持久化与运行时应用。
pub mod settings;

/// UI 子模块（D29）：主窗口、口令对话框与设置对话框；界面全部由 Rust 代码构建。
pub mod ui;

#[cfg(test)]
mod test_support;

use gtk::gio::prelude::{ApplicationExt, ApplicationExtManual};
use gtk4 as gtk;
use libadwaita as adw;

/// 应用 ID（非用户可见文案）。
pub const APP_ID: &str = "dev.rikki.MagiShield";

/// 启动应用（D28）：装载设置 → 解析界面语言 → 运行 `adw::Application`，GTK 就绪后应用主题。
pub fn run() -> gtk::glib::ExitCode {
    let settings = settings::Settings::load();
    rust_i18n::set_locale(settings.resolve_locale(current_env_tag().as_deref()));
    let app = adw::Application::builder().application_id(APP_ID).build();
    // 主题应用须在 GTK/libadwaita 初始化之后：`build()` 只构造应用对象，`adw_init` 发生在
    // 启动路径上。startup 信号（RUN_FIRST，默认处理器先跑 adw_init）早于任何窗口呈现，
    // 在此设置 AdwStyleManager，先于 activate 建窗、首帧即按所选主题渲染。
    app.connect_startup(move |_| settings::apply_theme(&settings));
    ui::MainWindow::install(&app);
    app.run()
}

/// 环境变量语言标签（D28 语言优先级链的中间级）：LC_ALL → LC_MESSAGES → LANG。
fn current_env_tag() -> Option<String> {
    std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .ok()
}

#[cfg(test)]
mod tests {
    /// 冒烟测试：确认 crate 可编译且元数据可见（脚手架验收，非行为测试）。
    #[test]
    fn test_crate_builds() {
        assert_eq!(env!("CARGO_PKG_NAME"), "magi-app");
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
    }

    /// 两份 locale 资源都随二进制内嵌（§6：默认 zh-CN，提供 en）。
    #[test]
    fn test_both_locales_are_embedded() {
        let available = rust_i18n::available_locales!();
        let names: Vec<String> = available.iter().map(|locale| locale.to_string()).collect();
        assert!(
            names.contains(&"zh-CN".to_string()),
            "缺少 zh-CN：{names:?}"
        );
        assert!(names.contains(&"en".to_string()), "缺少 en：{names:?}");
    }
}
