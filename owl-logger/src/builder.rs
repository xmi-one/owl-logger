use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use crate::config::*;
use crate::error::OwlError;
use crate::formatter;
use crate::guard::OwlGuard;
use crate::i18n::I18n;

/// owl-logger Builder（构建器）
///
/// 提供流畅的链式 API 来配置日志系统。
///
/// # 示例
///
/// ```rust,no_run
/// use owl_logger::{Language, LogLevel, RotationPolicy};
///
/// let _guard = owl_logger::builder()
///     .file_name("my_app")
///     .log_dir("logs")
///     .language(Language::Zh)
///     .level(LogLevel::Debug)
///     .rotation(RotationPolicy::Daily)
///     .init();
/// ```
pub struct OwlLoggerBuilder {
    config: OwlConfig,
}

impl OwlLoggerBuilder {
    /// 创建一个使用默认配置的 Builder
    pub fn new() -> Self {
        Self {
            config: OwlConfig::default(),
        }
    }

    /// 设置日志文件名前缀（不含扩展名）
    ///
    /// 默认值：`"app"`
    pub fn file_name(mut self, name: impl Into<String>) -> Self {
        self.config.file_name = name.into();
        self
    }

    /// 设置日志文件存放目录
    ///
    /// 默认值：`"logs"`
    pub fn log_dir(mut self, dir: impl Into<String>) -> Self {
        self.config.log_dir = dir.into();
        self
    }

    /// 设置最低日志级别
    ///
    /// 默认值：`LogLevel::Info`
    pub fn level(mut self, level: LogLevel) -> Self {
        self.config.level = level;
        self
    }

    /// 设置输出语言
    ///
    /// 默认值：`Language::En`
    pub fn language(mut self, lang: Language) -> Self {
        self.config.language = lang;
        self
    }

    /// 设置输出格式
    ///
    /// 默认值：`OutputFormat::Pretty`
    pub fn format(mut self, fmt: OutputFormat) -> Self {
        self.config.format = fmt;
        self
    }

    /// 设置文件轮转策略
    ///
    /// 默认值：`RotationPolicy::Daily`
    pub fn rotation(mut self, policy: RotationPolicy) -> Self {
        self.config.rotation = policy;
        self
    }

    /// 启用或禁用控制台输出
    ///
    /// 默认值：`true`
    pub fn console(mut self, enable: bool) -> Self {
        self.config.enable_console = enable;
        self
    }

    /// 启用或禁用文件输出
    ///
    /// 默认值：`true`
    pub fn file(mut self, enable: bool) -> Self {
        self.config.enable_file = enable;
        self
    }

    /// 启用或禁用 ANSI 彩色输出（仅影响控制台）
    ///
    /// 默认值：`true`
    pub fn ansi(mut self, enable: bool) -> Self {
        self.config.enable_ansi = enable;
        self
    }

    /// 是否显示日志来源模块路径
    ///
    /// 默认值：`true`
    pub fn show_target(mut self, show: bool) -> Self {
        self.config.show_target = show;
        self
    }

    /// 是否显示线程信息
    ///
    /// 默认值：`false`
    pub fn show_thread(mut self, show: bool) -> Self {
        self.config.show_thread = show;
        self
    }

    /// 是否显示源码行号
    ///
    /// 默认值：`false`
    pub fn show_line_number(mut self, show: bool) -> Self {
        self.config.show_line_number = show;
        self
    }

    /// 构建并初始化全局日志 subscriber
    ///
    /// 返回 `OwlGuard`，必须在 `main()` 中持有以确保日志不丢失。
    ///
    /// # Panics
    ///
    /// 如果全局 subscriber 已经被设置，将会 panic。
    /// 如果需要处理错误，请使用 `try_init()`。
    pub fn init(self) -> OwlGuard {
        self.try_init()
            .expect("owl-logger: failed to initialize. Is the global subscriber already set?")
    }

    /// 尝试构建并初始化全局日志 subscriber
    ///
    /// 与 `init()` 相同，但失败时返回 `Err` 而不是 panic。
    pub fn try_init(self) -> Result<OwlGuard, OwlError> {
        let config = self.config;

        // 构建环境过滤器：优先使用 RUST_LOG 环境变量，否则使用配置的级别
        let env_filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(config.level.to_string()));

        let mut console_guard: Option<tracing_appender::non_blocking::WorkerGuard> = None;
        let mut file_guard: Option<tracing_appender::non_blocking::WorkerGuard> = None;

        // 构建控制台层（Option）
        let console_layer = if config.enable_console {
            let (non_blocking, guard) = tracing_appender::non_blocking(std::io::stderr());
            console_guard = Some(guard);

            let fmt = formatter::console_formatter(config.language, &config);
            Some(
                tracing_subscriber::fmt::layer()
                    .with_writer(non_blocking)
                    .event_format(fmt)
                    .with_ansi(config.enable_ansi),
            )
        } else {
            None
        };

        // 构建文件层（Option）
        let file_layer = if config.enable_file {
            std::fs::create_dir_all(&config.log_dir)
                .map_err(OwlError::LogDirCreation)?;

            let file_appender = match &config.rotation {
                RotationPolicy::Daily => {
                    tracing_appender::rolling::daily(&config.log_dir, &config.file_name)
                }
                RotationPolicy::Hourly => {
                    tracing_appender::rolling::hourly(&config.log_dir, &config.file_name)
                }
                RotationPolicy::SizeMB(_) | RotationPolicy::Never => {
                    tracing_appender::rolling::never(
                        &config.log_dir,
                        format!("{}.log", &config.file_name),
                    )
                }
            };

            let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
            file_guard = Some(guard);

            let fmt = formatter::file_formatter(config.language, &config);
            Some(
                tracing_subscriber::fmt::layer()
                    .with_writer(non_blocking)
                    .event_format(fmt)
                    .with_ansi(false),
            )
        } else {
            None
        };

        // 使用 Option<Layer> 组合 — tracing-subscriber 原生支持 Option 作为 Layer
        // Option<Layer<S>> 自动实现 Layer<S>，None 时为空操作
        tracing_subscriber::registry()
            .with(env_filter)
            .with(console_layer)
            .with(file_layer)
            .init();

        // 桥接 log crate
        tracing_log::LogTracer::init().ok();

        // 设置全局语言状态供 #[monitor] 宏查询
        crate::__private::set_language(config.language);

        // 打印初始化成功消息
        tracing::info!("{}", I18n::init_message(config.language));

        Ok(OwlGuard {
            _file_guard: file_guard,
            _console_guard: console_guard,
            language: config.language,
        })
    }
}

impl Default for OwlLoggerBuilder {
    fn default() -> Self {
        Self::new()
    }
}
