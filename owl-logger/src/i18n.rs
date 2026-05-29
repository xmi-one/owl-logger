use tracing::Level;

use crate::config::Language;

/// 国际化（i18n）文本映射
///
/// 提供日志级别名称、系统提示语等的多语言支持。
#[allow(dead_code)]
pub(crate) struct I18n;

#[allow(dead_code)]
impl I18n {
    /// 获取日志级别的本地化名称
    pub fn level_name(level: &Level, lang: Language) -> &'static str {
        match (level, lang) {
            (&Level::TRACE, _) => "TRACE",
            (&Level::DEBUG, _) => "DEBUG",
            (&Level::INFO, _) => "INFO ",
            (&Level::WARN, _) => "WARN ",
            (&Level::ERROR, _) => "ERROR",
        }
    }

    /// 初始化完成提示
    pub fn init_message(lang: Language) -> &'static str {
        match lang {
            Language::Zh => "🦉 owl-logger 初始化完成",
            Language::En => "🦉 owl-logger initialized",
        }
    }

    /// 清理提示
    pub fn cleanup_message(lang: Language) -> &'static str {
        match lang {
            Language::Zh => "🦉 owl-logger 正在清理并持久化日志...",
            Language::En => "🦉 owl-logger flushing and cleaning up...",
        }
    }

    /// 进入函数提示（用于 #[monitor] 宏）
    pub fn entering_function(lang: Language) -> &'static str {
        match lang {
            Language::Zh => "→ 进入",
            Language::En => "→ entering",
        }
    }

    /// 退出函数提示（用于 #[monitor] 宏）
    pub fn exiting_function(lang: Language) -> &'static str {
        match lang {
            Language::Zh => "← 退出",
            Language::En => "← exiting",
        }
    }

    /// 耗时提示
    pub fn elapsed(lang: Language) -> &'static str {
        match lang {
            Language::Zh => "耗时",
            Language::En => "elapsed",
        }
    }
}
