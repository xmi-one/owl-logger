use std::fmt;

/// 日志级别（映射到 tracing::Level）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

impl LogLevel {
    /// 转换为 tracing 的 Level
    pub fn to_tracing_level(self) -> tracing::Level {
        match self {
            LogLevel::Trace => tracing::Level::TRACE,
            LogLevel::Debug => tracing::Level::DEBUG,
            LogLevel::Info => tracing::Level::INFO,
            LogLevel::Warn => tracing::Level::WARN,
            LogLevel::Error => tracing::Level::ERROR,
        }
    }

    /// 转换为 tracing_subscriber 的 LevelFilter
    pub fn to_level_filter(self) -> tracing_subscriber::filter::LevelFilter {
        match self {
            LogLevel::Trace => tracing_subscriber::filter::LevelFilter::TRACE,
            LogLevel::Debug => tracing_subscriber::filter::LevelFilter::DEBUG,
            LogLevel::Info => tracing_subscriber::filter::LevelFilter::INFO,
            LogLevel::Warn => tracing_subscriber::filter::LevelFilter::WARN,
            LogLevel::Error => tracing_subscriber::filter::LevelFilter::ERROR,
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogLevel::Trace => write!(f, "trace"),
            LogLevel::Debug => write!(f, "debug"),
            LogLevel::Info => write!(f, "info"),
            LogLevel::Warn => write!(f, "warn"),
            LogLevel::Error => write!(f, "error"),
        }
    }
}

/// 输出语言
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    #[default]
    En,
    Zh,
}

/// 日志输出格式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// 开发环境，带颜色和缩进
    #[default]
    Pretty,
    /// 精简单行
    Compact,
    /// 结构化 JSON（生产环境推荐）
    Json,
}

/// 文件轮转策略
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RotationPolicy {
    /// 按文件大小轮转（单位：兆字节）
    SizeMB(u64),
    /// 每天轮转
    #[default]
    Daily,
    /// 每小时轮转
    Hourly,
    /// 不轮转
    Never,
}

/// owl-logger 完整配置
#[derive(Debug, Clone)]
pub struct OwlConfig {
    /// 日志文件名前缀（不含扩展名）
    pub file_name: String,
    /// 日志文件存放目录
    pub log_dir: String,
    /// 最低日志级别
    pub level: LogLevel,
    /// 输出语言
    pub language: Language,
    /// 输出格式
    pub format: OutputFormat,
    /// 文件轮转策略
    pub rotation: RotationPolicy,
    /// 是否启用控制台输出
    pub enable_console: bool,
    /// 是否启用文件输出
    pub enable_file: bool,
    /// 是否启用 ANSI 彩色（控制台）
    pub enable_ansi: bool,
    /// 是否显示日志来源模块
    pub show_target: bool,
    /// 是否显示线程信息
    pub show_thread: bool,
    /// 是否显示源码行号
    pub show_line_number: bool,
}

impl Default for OwlConfig {
    fn default() -> Self {
        Self {
            file_name: "app".to_string(),
            log_dir: "logs".to_string(),
            level: LogLevel::default(),
            language: Language::default(),
            format: OutputFormat::default(),
            rotation: RotationPolicy::default(),
            enable_console: true,
            enable_file: true,
            enable_ansi: true,
            show_target: true,
            show_thread: false,
            show_line_number: false,
        }
    }
}
