//! D28 应用内设置：主题与语言的读取、持久化（glib KeyFile）与运行时应用。
//!
//! 持久化路径：`glib::user_config_dir()/magi/settings.ini`，组 `[ui]`，键 `language`/`theme`。
//! 读写失败一律回落默认值（默认深色主题、语言跟随系统）；设置文件只含两个枚举值（§6/AC-011）。

use std::path::{Path, PathBuf};

use gtk4 as gtk;
use libadwaita as adw;

/// 主题三态（D28）：默认深色（Samsung Magician 观感，§4.11 深色侧边栏判据）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemePreference {
    /// 跟随系统。
    System,
    /// 浅色。
    Light,
    /// 深色（默认）。
    #[default]
    Dark,
}

/// 语言三态（D28）：默认跟随系统（环境变量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LanguagePreference {
    /// 跟随系统（环境变量）。
    #[default]
    System,
    /// 中文。
    ZhCn,
    /// English。
    En,
}

impl ThemePreference {
    fn from_value(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }

    fn value(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

impl LanguagePreference {
    fn from_value(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "zh-CN" => Some(Self::ZhCn),
            "en" => Some(Self::En),
            _ => None,
        }
    }

    fn value(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::ZhCn => "zh-CN",
            Self::En => "en",
        }
    }
}

/// 应用内设置（D28）：两项选择；读取失败/字段非法时单项回落默认。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settings {
    pub theme: ThemePreference,
    pub language: LanguagePreference,
}

impl Settings {
    /// 从默认路径读取（不存在或损坏 → 全默认）。
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    /// 默认配置文件路径：`$XDG_CONFIG_HOME/magi/settings.ini`。
    pub fn path() -> PathBuf {
        gtk::glib::user_config_dir()
            .join("magi")
            .join("settings.ini")
    }

    /// 从指定路径读取（测试注入点）。
    fn load_from(path: &Path) -> Self {
        let key_file = gtk::glib::KeyFile::new();
        if key_file
            .load_from_file(path, gtk::glib::KeyFileFlags::NONE)
            .is_err()
        {
            return Self::default();
        }
        let theme = key_file
            .string("ui", "theme")
            .ok()
            .and_then(|value| ThemePreference::from_value(&value))
            .unwrap_or_default();
        let language = key_file
            .string("ui", "language")
            .ok()
            .and_then(|value| LanguagePreference::from_value(&value))
            .unwrap_or_default();
        Self { theme, language }
    }

    /// 写入默认路径；父目录缺失时创建；写失败只记诊断、不打断交互（内存态已生效）。
    pub fn save(&self) {
        if let Some(parent) = Self::path().parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let key_file = gtk::glib::KeyFile::new();
        key_file.set_string("ui", "theme", self.theme.value());
        key_file.set_string("ui", "language", self.language.value());
        if let Err(error) = key_file.save_to_file(Self::path()) {
            crate::diagnostics::ring().record(
                crate::diagnostics::Level::Warn,
                &format!("settings save failed: {error}"),
            );
        }
    }

    /// 主题 → 颜色方案映射（纯函数，可在无 GTK 环境断言）。
    pub fn color_scheme(&self) -> adw::ColorScheme {
        match self.theme {
            ThemePreference::System => adw::ColorScheme::Default,
            ThemePreference::Light => adw::ColorScheme::ForceLight,
            ThemePreference::Dark => adw::ColorScheme::ForceDark,
        }
    }

    /// 语言优先级（D28）：应用内显式选择 > 环境变量 > 默认 zh-CN。
    pub fn resolve_locale(&self, env_tag: Option<&str>) -> &'static str {
        match self.language {
            LanguagePreference::ZhCn => "zh-CN",
            LanguagePreference::En => "en",
            LanguagePreference::System => crate::presentation::locale_for_tag(env_tag),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 持久化往返：写入临时路径后读回，两个枚举值保持一致。
    #[test]
    fn test_settings_keyfile_roundtrip() {
        let dir = std::env::temp_dir().join(format!("magi-settings-test-{}", std::process::id()));
        let path = dir.join("settings.ini");
        let settings = Settings {
            theme: ThemePreference::Light,
            language: LanguagePreference::En,
        };
        let key_file = gtk::glib::KeyFile::new();
        key_file.set_string("ui", "theme", settings.theme.value());
        key_file.set_string("ui", "language", settings.language.value());
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        key_file.save_to_file(&path).expect("写入设置文件");
        assert_eq!(Settings::load_from(&path), settings);
        // 损坏文件 → 全默认，不 panic。
        std::fs::write(&path, b"not a key file {{{").expect("写入损坏文件");
        assert_eq!(Settings::load_from(&path), Settings::default());
        // 不存在的文件 → 全默认。
        assert_eq!(
            Settings::load_from(&dir.join("missing.ini")),
            Settings::default()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 语言优先级（D28）：显式选择 > 环境变量 > 默认 zh-CN。
    #[test]
    fn test_language_precedence() {
        let zh = Settings {
            language: LanguagePreference::ZhCn,
            ..Default::default()
        };
        assert_eq!(zh.resolve_locale(Some("en_US.UTF-8")), "zh-CN");
        let en = Settings {
            language: LanguagePreference::En,
            ..Default::default()
        };
        assert_eq!(en.resolve_locale(None), "en");
        let sys = Settings::default();
        assert_eq!(sys.resolve_locale(Some("en_US.UTF-8")), "en");
        assert_eq!(sys.resolve_locale(None), "zh-CN");
        assert_eq!(sys.resolve_locale(Some("ja_JP.UTF-8")), "zh-CN");
    }

    /// 默认主题为深色（D28 / §4.11 深色侧边栏判据）。
    #[test]
    fn test_theme_default_is_dark() {
        assert_eq!(Settings::default().theme, ThemePreference::Dark);
        assert_eq!(
            Settings::default().color_scheme(),
            adw::ColorScheme::ForceDark
        );
        assert_eq!(
            Settings {
                theme: ThemePreference::System,
                ..Default::default()
            }
            .color_scheme(),
            adw::ColorScheme::Default
        );
        assert_eq!(
            Settings {
                theme: ThemePreference::Light,
                ..Default::default()
            }
            .color_scheme(),
            adw::ColorScheme::ForceLight
        );
    }
}

/// 应用主题到全局样式管理器（D28；T4b 自 ui/mod 归位到设置域）。
///
/// 须在 GTK 初始化之后调用（`AdwApplication` 的 `startup` 阶段及以后），否则
/// `StyleManager::default()` 会触发「Gtk has to be initialized」断言。
pub fn apply_theme(settings: &Settings) {
    libadwaita::StyleManager::default().set_color_scheme(settings.color_scheme());
}
