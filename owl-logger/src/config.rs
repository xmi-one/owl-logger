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
    /// 时间戳格式化字符串
    pub time_format: String,
    /// 是否使用 UTC 时区
    pub use_utc: bool,
    /// 最大日志文件保留数
    pub max_files: Option<usize>,
    /// 是否捕获 Panic 并通过日志输出
    pub catch_panic: bool,
    /// 全局常量字段（会自动附加到每条日志）
    pub global_fields: std::collections::HashMap<String, String>,
    /// 敏感字段名列表（包含的字段内容会被自动脱敏为 [MASKED]）
    pub sensitive_keys: Vec<String>,
    /// 日志保留天数（超过此天数的日志文件会被自动清理）
    pub retention_days: Option<usize>,
    /// 异步非阻塞队列的容量限制
    pub buffered_lines_limit: usize,
    /// 队列满时是否允许丢弃日志（false 则阻塞当前线程）
    pub lossy: bool,
    /// 按级别分离的独立日志文件阈值。
    ///
    /// 若设置为 `Some(level)`，则会额外写入一个 `{file_name}.{level}.log` 文件，
    /// 仅记录达到或严重于该级别的日志（例如 `Error` 仅记录 ERROR，`Warn` 记录 WARN+ERROR）。
    pub error_file_level: Option<LogLevel>,
    /// OTLP 导出端点（OTLP/HTTP，如 `http://localhost:4318/v1/traces`）。
    ///
    /// 仅在启用 `otlp` feature 时生效；为 `None` 时不导出。
    pub otlp_endpoint: Option<String>,
    /// OTLP 导出时上报的服务名（`service.name` 资源属性）。
    ///
    /// 为 `None` 时回退使用 `file_name`。仅在启用 `otlp` feature 时生效。
    pub otlp_service_name: Option<String>,
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
            time_format: "%Y-%m-%d %H:%M:%S%.3f".to_string(),
            use_utc: false,
            max_files: None,
            catch_panic: false,
            global_fields: std::collections::HashMap::new(),
            sensitive_keys: vec![
                "password".to_string(),
                "token".to_string(),
                "secret".to_string(),
                "authorization".to_string(),
                "credit_card".to_string(),
            ],
            retention_days: Some(7),
            buffered_lines_limit: 120_000,
            lossy: true,
            error_file_level: None,
            otlp_endpoint: None,
            otlp_service_name: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OwlConfig;

    #[test]
    fn panic_hook_is_opt_in_by_default() {
        assert!(!OwlConfig::default().catch_panic);
    }
}
