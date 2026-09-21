//! 仪表盘页（D29 分文件）：页面构建与设备分组/进度与结果呈现；编排仍经 MainWindow。

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use magi_protocol::UnlockStep;
use rust_i18n::t;

use crate::diagnostics::{self, Level};
use crate::device::jobs::ScanHit;
use crate::device::rescan::DeviceIdentity;
use crate::presentation::{self, AppError};
use crate::ui::MainWindow;
use crate::ui::main_window::{Outcome, StoredOutcome, badge_class, result_kind, status_icon_name};

/// 仪表盘控件集（构建于本文件，装配进主窗口 imp）。
pub(crate) struct DashboardWidgets {
    pub toast_overlay: adw::ToastOverlay,
    pub device_group: adw::PreferencesGroup,
    pub device_row: adw::ActionRow,
    /// 设备图标（构建即挂入设备行前缀；句柄由控件集完整持有，暂无呈现读取点）。
    #[allow(dead_code)]
    pub device_icon: gtk::Image,
    pub status_icon: gtk::Image,
    pub status_label: gtk::Label,
    pub badge_box: gtk::Box,
    pub node_row: adw::ActionRow,
    pub channel_row: adw::ActionRow,
    pub descriptor_row: adw::ActionRow,
    pub actions_group: adw::PreferencesGroup,
    pub action_unlock: adw::ButtonRow,
    pub action_validate_password: adw::ButtonRow,
    pub action_set_password: adw::ButtonRow,
    pub action_change_password: adw::ButtonRow,
    pub action_delete_password: adw::ButtonRow,
    pub feedback_group: adw::PreferencesGroup,
    pub progress_box: gtk::Box,
    pub progress: gtk::ProgressBar,
    pub action_cancel: gtk::Button,
    pub result_row: gtk::Box,
    pub result_icon: gtk::Image,
    pub result_label: gtk::Label,
}

/// 构建仪表盘页（返回页面根 = 包裹 PreferencesPage 的 ToastOverlay 与控件集）。
pub(crate) fn build() -> (gtk::Widget, DashboardWidgets) {
    // 设备分组：设备行（徽章在行尾）+ 三条信息行。
    let device_icon = gtk::Image::from_icon_name("drive-harddisk-symbolic");
    device_icon.set_pixel_size(32);
    device_icon.add_css_class("dim-label");
    let status_icon = gtk::Image::builder().pixel_size(16).build();
    let status_label = gtk::Label::new(None);
    status_label.add_css_class("heading");
    let badge_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    badge_box.set_valign(gtk::Align::Center);
    badge_box.append(&status_icon);
    badge_box.append(&status_label);

    let device_row = adw::ActionRow::new();
    device_row.add_prefix(&device_icon);
    device_row.add_suffix(&badge_box);

    let node_row = adw::ActionRow::new();
    let channel_row = adw::ActionRow::new();
    let descriptor_row = adw::ActionRow::new();

    let device_group = adw::PreferencesGroup::new();
    device_group.add(&device_row);
    device_group.add(&node_row);
    device_group.add(&channel_row);
    device_group.add(&descriptor_row);

    let dashboard_page = adw::PreferencesPage::new();
    dashboard_page.add(&device_group);

    // 操作分组：五个 AdwButtonRow（文案与敏感度在装配时装配）。
    let actions_group = adw::PreferencesGroup::new();
    let action_unlock = adw::ButtonRow::new();
    action_unlock.set_start_icon_name(Some("system-lock-screen-symbolic"));
    let action_validate_password = adw::ButtonRow::new();
    action_validate_password.set_start_icon_name(Some("dialog-password-symbolic"));
    let action_set_password = adw::ButtonRow::new();
    action_set_password.set_start_icon_name(Some("document-new-symbolic"));
    let action_change_password = adw::ButtonRow::new();
    action_change_password.set_start_icon_name(Some("document-edit-symbolic"));
    let action_delete_password = adw::ButtonRow::new();
    action_delete_password.set_start_icon_name(Some("edit-delete-symbolic"));
    actions_group.add(&action_unlock);
    actions_group.add(&action_validate_password);
    actions_group.add(&action_set_password);
    actions_group.add(&action_change_password);
    actions_group.add(&action_delete_password);
    dashboard_page.add(&actions_group);

    // 进度与结果分组。
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

    let feedback_group = adw::PreferencesGroup::new();
    feedback_group.add(&progress_box);
    feedback_group.add(&result_row);
    dashboard_page.add(&feedback_group);

    // Toast 承载层：成功结果的瞬时提示（libadwaita 特色，独立小步）。
    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&dashboard_page));
    (
        toast_overlay.clone().upcast(),
        DashboardWidgets {
            toast_overlay,
            device_group,
            device_row,
            device_icon,
            status_icon,
            status_label,
            badge_box,
            node_row,
            channel_row,
            descriptor_row,
            actions_group,
            action_unlock,
            action_validate_password,
            action_set_password,
            action_change_password,
            action_delete_password,
            feedback_group,
            progress_box,
            progress,
            action_cancel,
            result_row,
            result_icon,
            result_label,
        },
    )
}

impl MainWindow {
    /// 设备分组（§4.12：产品名、VID:PID、设备节点、传输通道、描述符摘要与锁定徽章）。
    pub(crate) fn show_hit(&self, hit: &ScanHit) {
        let imp = self.imp();
        // relocalize 重放缓存：语言切换后按新语言重放设备分组文案。
        self.imp().replay.borrow_mut().last_hit = Some(hit.clone());
        imp.dashboard.device_row.set_title(&t!("device.model"));
        let ids = format!(
            "{} {:04x}:{:04x}",
            t!("device.vid_pid"),
            hit.job.vid,
            hit.job.pid
        );
        imp.dashboard.device_row.set_subtitle(ids.as_str());
        imp.dashboard
            .status_label
            .set_label(&t!(hit.job.identity.status_key()));
        self.set_badge_class(hit.job.identity);
        imp.dashboard.badge_box.set_visible(true);
        let node = match &hit.job.node {
            Some(node) => node.to_string(),
            None => String::new(),
        };
        imp.dashboard.node_row.set_subtitle(node.as_str());
        imp.dashboard
            .channel_row
            .set_subtitle(t!("device.channel").as_ref());
        imp.dashboard
            .descriptor_row
            .set_subtitle(hit.descriptor.as_deref().unwrap_or(""));
        for row in [
            &imp.dashboard.node_row,
            &imp.dashboard.channel_row,
            &imp.dashboard.descriptor_row,
        ] {
            row.set_visible(true);
        }
    }

    /// 设备分组空态（§4.1：不识别为 T7 Shield，入口保持禁用）。
    pub(crate) fn show_unknown_device(&self) {
        let imp = self.imp();
        // 空态与设备分组互斥：清除重放缓存，relocalize 才会重放空态而不是陈旧设备行。
        self.imp().replay.borrow_mut().last_hit = None;
        imp.dashboard
            .device_row
            .set_title(&t!(DeviceIdentity::Unrecognized.status_key()));
        imp.dashboard
            .device_row
            .set_subtitle(t!("device.empty_hint").as_ref());
        imp.dashboard
            .status_label
            .set_label(&t!(DeviceIdentity::Unrecognized.status_key()));
        self.set_badge_class(DeviceIdentity::Unrecognized);
        // 空态行标题已是「未发现 T7 Shield」，徽章不再重复呈现。
        imp.dashboard.badge_box.set_visible(false);
        for row in [
            &imp.dashboard.node_row,
            &imp.dashboard.channel_row,
            &imp.dashboard.descriptor_row,
        ] {
            row.set_subtitle("");
            row.set_visible(false);
        }
    }

    /// 状态徽章配色（锁定态醒目暖色、解锁态绿色、其余中性）。
    pub(crate) fn set_badge_class(&self, identity: DeviceIdentity) {
        let imp = self.imp();
        for class in ["warning", "success", "dim-label"] {
            imp.dashboard.status_label.remove_css_class(class);
            imp.dashboard.status_icon.remove_css_class(class);
        }
        let class = badge_class(identity);
        imp.dashboard.status_label.add_css_class(class);
        imp.dashboard.status_icon.add_css_class(class);
        imp.dashboard
            .status_icon
            .set_icon_name(Some(status_icon_name(identity)));
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
            imp.dashboard.result_label.remove_css_class(class);
        }
        imp.dashboard.result_icon.set_visible(false);
        imp.dashboard.feedback_group.set_visible(true);
        imp.dashboard.result_row.set_visible(true);
        imp.dashboard.progress_box.set_visible(true);
        imp.dashboard.progress.set_fraction(done / total);
        imp.dashboard
            .result_label
            .set_label(&t!(format!("progress.{step}")));
    }

    /// 结果区统一呈现（§4.12/§6）：文本 + 语义类 + 图标；进度条随结果/错误隐藏。
    pub(crate) fn show_outcome(&self, text: &str, kind: Outcome) {
        let imp = self.imp();
        for class in ["success", "error", "dim-label"] {
            imp.dashboard.result_label.remove_css_class(class);
            imp.dashboard.result_icon.remove_css_class(class);
        }
        imp.dashboard.feedback_group.set_visible(true);
        imp.dashboard.result_row.set_visible(true);
        imp.dashboard.result_label.set_label(text);
        match kind {
            Outcome::Neutral => {
                imp.dashboard.result_label.add_css_class("dim-label");
                imp.dashboard.result_icon.set_visible(false);
            }
            Outcome::Success => {
                imp.dashboard.result_label.add_css_class("success");
                imp.dashboard.result_icon.add_css_class("success");
                imp.dashboard
                    .result_icon
                    .set_icon_name(Some("emblem-ok-symbolic"));
                imp.dashboard.result_icon.set_visible(true);
                // 成功结果附 Toast 瞬时提示（libadwaita 特色）。
                imp.dashboard
                    .toast_overlay
                    .add_toast(adw::Toast::new(text));
            }
            Outcome::Error => {
                imp.dashboard.result_label.add_css_class("error");
                imp.dashboard.result_icon.add_css_class("error");
                imp.dashboard
                    .result_icon
                    .set_icon_name(Some("dialog-warning-symbolic"));
                imp.dashboard.result_icon.set_visible(true);
            }
        }
        imp.dashboard.progress.set_fraction(0.0);
        imp.dashboard.progress_box.set_visible(false);
    }

    /// 空闲态：隐藏结果区（无进度 / 无结果 / 无错误时不占版面）。
    pub(crate) fn hide_outcome(&self) {
        let imp = self.imp();
        for class in ["success", "error", "dim-label"] {
            imp.dashboard.result_label.remove_css_class(class);
            imp.dashboard.result_icon.remove_css_class(class);
        }
        imp.dashboard.result_icon.set_visible(false);
        imp.dashboard.result_row.set_visible(false);
        imp.dashboard.progress.set_fraction(0.0);
        imp.dashboard.progress_box.set_visible(false);
        // 无进度 / 无结果时不保留空分组（标题悬空是主要的空荡感来源）。
        imp.dashboard.feedback_group.set_visible(false);
    }

    /// 结果文案（§4.8 判据分级 / 取消 / 校验结论）。
    pub fn show_result(&self, message_key: &'static str) {
        // relocalize 重放缓存（success/neutral 均记；重放时重复写同一值，幂等无害）。
        self.imp().replay.borrow_mut().last_outcome = Some(StoredOutcome::Result(message_key));
        self.show_outcome(&t!(message_key), result_kind(message_key));
    }

    /// 错误呈现：呈现码 + 一句原因 + 一句建议（§4.13）。
    pub fn show_error(&self, error: &AppError) -> String {
        // relocalize 重放缓存（错误经呈现码/原因/建议键重放）。
        self.imp().replay.borrow_mut().last_outcome = Some(StoredOutcome::Error(error.clone()));
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
}
