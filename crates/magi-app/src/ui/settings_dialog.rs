//! 设置对话框（D28/D29/D30：界面由 Rust 代码构建，使用 libadwaita 原生组件）。
//!
//! 三组能力：
//! - 外观（`AdwComboRow` × 2）：主题与语言两组三态，更改即时生效并持久化——
//!   主题经 [`crate::ui::apply_theme`]，语言经 `rust_i18n::set_locale` 与双向 `relocalize()`；
//! - 工作日志（`AdwButtonRow`）：脱敏导出入口（D30：原诊断页移除后唯一的日志出口）。
//!
//! 候选项文案经 `gtk::StringList` + `t!()` 在本模块构造（AC-015：不内联可显示字符串）。

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use rust_i18n::t;

use crate::settings::{LanguagePreference, Settings, ThemePreference};
use crate::ui::MainWindow;

mod imp {
    use super::*;

    /// 设置对话框子件（D29：全部在 Rust 侧构建）。
    pub struct SettingsDialog {
        pub page: adw::PreferencesPage,
        pub appearance_group: adw::PreferencesGroup,
        pub theme_row: adw::ComboRow,
        pub language_row: adw::ComboRow,
        pub diagnostics_group: adw::PreferencesGroup,
        pub export_row: adw::ButtonRow,
        pub(crate) settings: RefCell<Settings>,
        pub(crate) window: RefCell<Option<glib::WeakRef<MainWindow>>>,
        /// 装配期间屏蔽 notify::selected 回调（初始 selected 写入不应触发持久化）。
        pub(crate) loading: Cell<bool>,
    }

    impl Default for SettingsDialog {
        fn default() -> Self {
            let theme_row = adw::ComboRow::new();
            let language_row = adw::ComboRow::new();
            let appearance_group = adw::PreferencesGroup::new();
            appearance_group.add(&theme_row);
            appearance_group.add(&language_row);
            let export_row = adw::ButtonRow::new();
            export_row.set_start_icon_name(Some("document-save-symbolic"));
            let diagnostics_group = adw::PreferencesGroup::new();
            diagnostics_group.add(&export_row);
            let page = adw::PreferencesPage::new();
            page.add(&appearance_group);
            page.add(&diagnostics_group);
            Self {
                page,
                appearance_group,
                theme_row,
                language_row,
                diagnostics_group,
                export_row,
                settings: RefCell::new(Settings::default()),
                window: RefCell::new(None),
                loading: Cell::new(false),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SettingsDialog {
        const NAME: &'static str = "MagiSettingsDialog";
        type Type = super::SettingsDialog;
        type ParentType = adw::PreferencesDialog;
    }

    impl ObjectImpl for SettingsDialog {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_content_width(420);
            obj.set_content_height(360);
            obj.add(&self.page);
        }
    }

    impl WidgetImpl for SettingsDialog {}
    impl adw::subclass::prelude::AdwDialogImpl for SettingsDialog {}
    impl adw::subclass::prelude::PreferencesDialogImpl for SettingsDialog {}
}

glib::wrapper! {
    /// 设置对话框（D28）。
    pub struct SettingsDialog(ObjectSubclass<imp::SettingsDialog>)
        @extends adw::PreferencesDialog, adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

/// 主题三态在 ComboRow 中的行序（0/1/2）。
fn theme_index(theme: ThemePreference) -> u32 {
    match theme {
        ThemePreference::System => 0,
        ThemePreference::Light => 1,
        ThemePreference::Dark => 2,
    }
}

fn theme_from_index(index: u32) -> ThemePreference {
    match index {
        1 => ThemePreference::Light,
        2 => ThemePreference::Dark,
        _ => ThemePreference::System,
    }
}

/// 语言三态在 ComboRow 中的行序（0/1/2）。
fn language_index(language: LanguagePreference) -> u32 {
    match language {
        LanguagePreference::System => 0,
        LanguagePreference::ZhCn => 1,
        LanguagePreference::En => 2,
    }
}

fn language_from_index(index: u32) -> LanguagePreference {
    match index {
        1 => LanguagePreference::ZhCn,
        2 => LanguagePreference::En,
        _ => LanguagePreference::System,
    }
}

/// 单键取文案（运行时键查询，键名集中在调用点可见）。
fn tr(key: &'static str) -> String {
    t!(key).to_string()
}

/// 三个候选项装进 `StringList`（键序 = 行序；语言 autonym 键两种界面语言下同值）。
fn option_list(keys: [&'static str; 3]) -> gtk::StringList {
    let labels: [String; 3] = keys.map(tr);
    gtk::StringList::new(&[labels[0].as_str(), labels[1].as_str(), labels[2].as_str()])
}

impl SettingsDialog {
    /// 构造对话框：装配文案、候选项与当前选中，并接线即时生效回调。
    pub fn new(window: &MainWindow, settings: Settings) -> Self {
        let dialog: Self = glib::Object::new();
        *dialog.imp().window.borrow_mut() = Some(window.downgrade());
        *dialog.imp().settings.borrow_mut() = settings;
        // 候选项为 GtkStringObject：固定属性表达式取 `string` 字段渲染（与模型替换无关，设一次）。
        let expression = gtk::PropertyExpression::new(
            gtk::StringObject::static_type(),
            None::<&gtk::Expression>,
            "string",
        );
        dialog.imp().theme_row.set_expression(Some(&expression));
        dialog.imp().language_row.set_expression(Some(&expression));
        dialog.imp().loading.set(true);
        dialog.relocalize();
        dialog
            .imp()
            .theme_row
            .set_selected(theme_index(settings.theme));
        dialog
            .imp()
            .language_row
            .set_selected(language_index(settings.language));
        dialog.imp().loading.set(false);
        dialog.connect_rows();
        dialog.connect_export();
        dialog
    }

    /// 语言切换后重设对话框自身文案（组/行标题与候选项，选中态保持）。
    pub fn relocalize(&self) {
        let imp = self.imp();
        self.set_title(&t!("settings.title"));
        imp.appearance_group.set_title(&t!("settings.group"));
        imp.theme_row.set_title(&t!("settings.theme"));
        imp.language_row.set_title(&t!("settings.language"));
        imp.diagnostics_group
            .set_title(&t!("settings.diagnostics_title"));
        imp.export_row.set_title(&t!("action.export_diagnostics"));
        let theme = theme_from_index(imp.theme_row.selected());
        let language = language_from_index(imp.language_row.selected());
        imp.loading.set(true);
        imp.theme_row.set_model(Some(&option_list([
            "settings.theme_system",
            "settings.theme_light",
            "settings.theme_dark",
        ])));
        imp.theme_row.set_selected(theme_index(theme));
        imp.language_row.set_model(Some(&option_list([
            "settings.lang_system",
            "settings.lang_zh",
            "settings.lang_en",
        ])));
        imp.language_row.set_selected(language_index(language));
        imp.loading.set(false);
    }

    /// 导出工作日志（D30：设置对话框内的唯一日志出口，脱敏后落盘）。
    fn connect_export(&self) {
        let dialog = self.clone();
        self.imp().export_row.connect_activated(move |_| {
            let window = dialog
                .imp()
                .window
                .borrow()
                .as_ref()
                .and_then(|weak| weak.upgrade());
            if let Some(window) = window {
                window.export_diagnostics();
            }
        });
    }

    /// 选择回调：即时应用 + 持久化 + 主窗口重渲染（装配期屏蔽）。
    fn connect_rows(&self) {
        let dialog = self.clone();
        self.imp().theme_row.connect_selected_notify(move |row| {
            let imp = dialog.imp();
            if imp.loading.get() {
                return;
            }
            let mut settings = imp.settings.borrow_mut();
            settings.theme = theme_from_index(row.selected());
            crate::ui::apply_theme(&settings);
            settings.save();
        });
        let dialog = self.clone();
        self.imp().language_row.connect_selected_notify(move |row| {
            let imp = dialog.imp();
            if imp.loading.get() {
                return;
            }
            let env_tag = std::env::var("LC_ALL")
                .or_else(|_| std::env::var("LC_MESSAGES"))
                .or_else(|_| std::env::var("LANG"))
                .ok();
            let window = imp.window.borrow().as_ref().and_then(|weak| weak.upgrade());
            {
                let mut settings = imp.settings.borrow_mut();
                settings.language = language_from_index(row.selected());
                rust_i18n::set_locale(settings.resolve_locale(env_tag.as_deref()));
                settings.save();
            }
            dialog.relocalize();
            if let Some(window) = window {
                window.relocalize();
            }
        });
    }

}
