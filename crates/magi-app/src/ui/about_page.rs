//! 关于页（D29 分文件）：版本/支持设备/协议参考三行与平台说明的构建与呈现。

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use rust_i18n::t;

use crate::ui::MainWindow;

/// 关于页控件集（构建于本文件，装配进主窗口 imp）。
pub(crate) struct AboutWidgets {
    pub about_group: adw::PreferencesGroup,
    pub about_version_row: adw::ActionRow,
    pub about_device_row: adw::ActionRow,
    pub about_repository_row: adw::ActionRow,
    pub platform_group: adw::PreferencesGroup,
    pub platform_row: adw::ActionRow,
}

/// 构建关于页（返回页面根控件与控件集）。
pub(crate) fn build() -> (gtk::Widget, AboutWidgets) {
    // 关于页：版本 / 支持设备 / 协议参考。
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
    (
        about_page.upcast(),
        AboutWidgets {
            about_group,
            about_version_row,
            about_device_row,
            about_repository_row,
            platform_group,
            platform_row,
        },
    )
}

impl MainWindow {
    /// 平台说明文案（D27：Linux 通道说明；置于关于页「运行环境」分组）。
    pub fn show_platform_notice(&self) {
        let text = t!("platform.linux_notice").to_string();
        self.imp().about.platform_row.set_subtitle(text.as_str());
    }
}
