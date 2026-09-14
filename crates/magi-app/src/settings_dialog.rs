//! D28 设置对话框（`CompositeTemplate`）：主题/语言三态选择，更改即时生效并持久化。
//!
//! 候选项文案经 `gtk::StringList` + `t!()` 在 Rust 侧构造（`.ui` 零字面量，K5）；
//! 语言切换后对话框自身与主窗口同步重渲染（relocalize）。

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;
use gtk::CompositeTemplate;
use gtk4 as gtk;
use libadwaita as adw;
use rust_i18n::t;

use crate::main_window::MainWindow;
use crate::settings::{LanguagePreference, Settings, ThemePreference};

mod imp {
    use super::*;

    #[derive(CompositeTemplate, Default)]
    #[template(file = "ui/settings_dialog.ui")]
    pub struct SettingsDialog {
        #[template_child]
        pub appearance_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub theme_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub language_row: TemplateChild<adw::ComboRow>,
        pub(crate) settings: std::cell::RefCell<Settings>,
        pub(crate) window: std::cell::RefCell<Option<glib::WeakRef<MainWindow>>>,
        /// 装配期间屏蔽 notify::selected 回调（初始 selected 写入不应触发持久化）。
        pub(crate) loading: std::cell::Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SettingsDialog {
        const NAME: &'static str = "MagiSettingsDialog";
        type Type = super::SettingsDialog;
        type ParentType = adw::PreferencesDialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for SettingsDialog {}
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
        dialog
    }

    /// 语言切换后重设对话框自身文案（组/行标题与候选项，选中态保持）。
    pub fn relocalize(&self) {
        let imp = self.imp();
        self.set_title(&t!("settings.title"));
        imp.appearance_group.set_title(&t!("settings.group"));
        imp.theme_row.set_title(&t!("settings.theme"));
        imp.language_row.set_title(&t!("settings.language"));
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
            settings.apply_theme();
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

    /// 当前设置快照（主窗口启动时装配初始态用）。
    pub fn settings(&self) -> Settings {
        *self.imp().settings.borrow()
    }
}
