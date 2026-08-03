use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use crate::config::*;
use crate::error::OwlError;
use crate::formatter;
use crate::guard::OwlGuard;
use crate::i18n::I18n;
use chrono::Timelike;

/// 擦除 subscriber 类型后的可重载过滤器。
///
/// 这让全局动态过滤 API 同时支持默认 Registry 和调用方预先组合的 subscriber。
pub(crate) trait ReloadableFilter: Send + Sync {
    fn current_filter(&self) -> Result<String, String>;
    fn reload_filter(&self, filter: EnvFilter) -> Result<(), String>;
}

impl<S> ReloadableFilter for tracing_subscriber::reload::Handle<EnvFilter, S>
where
    S: tracing::Subscriber + Send + Sync + 'static,
{
    fn current_filter(&self) -> Result<String, String> {
        self.with_current(|filter| filter.to_string())
            .map_err(|error| error.to_string())
    }

    fn reload_filter(&self, filter: EnvFilter) -> Result<(), String> {
        self.reload(filter).map_err(|error| error.to_string())
    }
}

/// 全局日志过滤器重载句柄，用于在运行期修改日志过滤器级别。
pub(crate) static RELOAD_HANDLE: std::sync::OnceLock<Box<dyn ReloadableFilter>> =
    std::sync::OnceLock::new();

/// 周期性保留期清理任务。
///
/// 由 OwlGuard 持有；Drop 时发送停止信号并等待线程退出，避免 logger 已关闭后仍有
/// 清理线程访问日志目录。
pub(crate) struct CleanupWorker {
    stop_sender: Option<std::sync::mpsc::Sender<()>>,
    join_handle: Option<std::thread::JoinHandle<()>>,
}

impl CleanupWorker {
    fn start(log_dir: std::path::PathBuf, file_name: String, retention_days: usize) -> Self {
        let (stop_sender, stop_receiver) = std::sync::mpsc::channel();
        let join_handle = std::thread::spawn(move || loop {
            match stop_receiver.recv_timeout(std::time::Duration::from_secs(3600)) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    cleanup_old_logs(&log_dir, &file_name, retention_days);
                }
            }
        });

        Self {
            stop_sender: Some(stop_sender),
            join_handle: Some(join_handle),
        }
    }
}

impl Drop for CleanupWorker {
    fn drop(&mut self) {
        self.stop_sender.take();
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

/// owl-logger Builder（构建器）
///
/// 提供流畅的链式 API 来配置日志系统。
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

    /// 从环境变量创建 Builder
    pub fn from_env() -> Result<Self, OwlError> {
        let mut builder = Self::new();

        if let Ok(level) = std::env::var("OWL_LOG_LEVEL") {
            builder = builder.level(parse_log_level(&level)?);
        }
        if let Ok(format) = std::env::var("OWL_LOG_FORMAT") {
            builder = builder.format(parse_output_format(&format)?);
        }
        if let Ok(log_dir) = std::env::var("OWL_LOG_DIR") {
            builder = builder.log_dir(log_dir);
        }
        if let Ok(file_name) = std::env::var("OWL_LOG_FILE") {
            builder = builder.file_name(file_name);
        }

        validate_config(&builder.config)?;
        Ok(builder)
    }

    /// 设置日志文件名前缀（不含扩展名）
    pub fn file_name(mut self, name: impl Into<String>) -> Self {
        self.config.file_name = name.into();
        self
    }

    /// 设置日志文件存放目录
    pub fn log_dir(mut self, dir: impl Into<String>) -> Self {
        self.config.log_dir = dir.into();
        self
    }

    /// 设置最低日志级别
    pub fn level(mut self, level: LogLevel) -> Self {
        self.config.level = level;
        self
    }

    /// 设置输出语言
    pub fn language(mut self, lang: Language) -> Self {
        self.config.language = lang;
        self
    }

    /// 设置输出格式
    pub fn format(mut self, fmt: OutputFormat) -> Self {
        self.config.format = fmt;
        self
    }

    /// 设置文件轮转策略
    pub fn rotation(mut self, policy: RotationPolicy) -> Self {
        self.config.rotation = policy;
        self
    }

    /// 启用或禁用控制台输出
    pub fn console(mut self, enable: bool) -> Self {
        self.config.enable_console = enable;
        self
    }

    /// 启用或禁用文件输出
    pub fn file(mut self, enable: bool) -> Self {
        self.config.enable_file = enable;
        self
    }

    /// 启用或禁用 ANSI 彩色输出（仅影响控制台）
    pub fn ansi(mut self, enable: bool) -> Self {
        self.config.enable_ansi = enable;
        self
    }

    /// 是否显示日志来源模块路径
    pub fn show_target(mut self, show: bool) -> Self {
        self.config.show_target = show;
        self
    }

    /// 是否显示线程信息
    pub fn show_thread(mut self, show: bool) -> Self {
        self.config.show_thread = show;
        self
    }

    /// 是否显示源码行号
    pub fn show_line_number(mut self, show: bool) -> Self {
        self.config.show_line_number = show;
        self
    }

    /// 设置时间戳格式字符串
    pub fn time_format(mut self, format: impl Into<String>) -> Self {
        self.config.time_format = format.into();
        self
    }

    /// 设置是否使用 UTC 时区代替本地时区
    pub fn utc(mut self, use_utc: bool) -> Self {
        self.config.use_utc = use_utc;
        self
    }

    /// 设置最大日志文件保留数量
    ///
    /// 上限包含当前活跃日志文件；传入 `0` 表示不限制数量。
    pub fn max_files(mut self, max_files: usize) -> Self {
        self.config.max_files = Some(max_files);
        self
    }

    /// 设置是否捕获 Panic 并通过日志输出。
    ///
    /// 该选项会替换进程级 panic hook，因此默认关闭。建议只在应用程序入口显式启用，
    /// 不要由可复用库开启。
    pub fn catch_panic(mut self, catch: bool) -> Self {
        self.config.catch_panic = catch;
        self
    }

    /// 添加全局属性字段
    pub fn global_field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.global_fields.insert(key.into(), value.into());
        self
    }

    /// 设置日志文件保留天数
    pub fn retention_days(mut self, days: usize) -> Self {
        self.config.retention_days = Some(days);
        self
    }

    /// 启用按级别分离的独立日志文件。
    ///
    /// 开启后会额外写入一个 `{file_name}.{level}.log` 文件，仅记录达到或严重于
    /// `min_level` 的日志，便于运维快速定位错误。例如：
    /// - `error_file(LogLevel::Error)` → `app.error.log` 仅含 ERROR
    /// - `error_file(LogLevel::Warn)`  → `app.warn.log` 含 WARN 与 ERROR
    pub fn error_file(mut self, min_level: LogLevel) -> Self {
        self.config.error_file_level = Some(min_level);
        self
    }

    /// 设置 OTLP 导出端点（OTLP/HTTP，如 `http://localhost:4318/v1/traces`）。
    ///
    /// 需要启用 `otlp` feature 才会真正导出；未启用该 feature 时此设置不生效。
    pub fn otlp_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.config.otlp_endpoint = Some(endpoint.into());
        self
    }

    /// 设置 OTLP 导出时的服务名（`service.name`）。未设置时回退使用文件名前缀。
    ///
    /// 需要启用 `otlp` feature 才会真正生效。
    pub fn otlp_service_name(mut self, name: impl Into<String>) -> Self {
        self.config.otlp_service_name = Some(name.into());
        self
    }

    /// 设置异步队列的缓冲行数上限
    pub fn buffered_lines_limit(mut self, limit: usize) -> Self {
        self.config.buffered_lines_limit = limit;
        self
    }

    /// 设置缓冲区写满时是否丢弃日志条目
    pub fn lossy(mut self, lossy: bool) -> Self {
        self.config.lossy = lossy;
        self
    }

    /// 构建并初始化全局日志 subscriber
    pub fn init(self) -> OwlGuard {
        self.try_init()
            .expect("owl-logger: failed to initialize. Is the global subscriber already set?")
    }

    /// 使用调用方提供的 subscriber 构建并初始化全局日志系统。
    ///
    /// 可先将应用自定义的 tracing Layer 叠加到 Registry，再交给 owl-logger 添加格式化、
    /// 文件输出和动态过滤能力。该方法与 init 一样会安装全局 subscriber。
    pub fn init_with_subscriber<S>(self, subscriber: S) -> OwlGuard
    where
        S: tracing::Subscriber
            + for<'a> tracing_subscriber::registry::LookupSpan<'a>
            + Send
            + Sync
            + 'static,
    {
        self.try_init_with_subscriber(subscriber)
            .expect("owl-logger: failed to initialize. Is the global subscriber already set?")
    }

    /// 尝试构建并初始化全局日志 subscriber
    pub fn try_init(self) -> Result<OwlGuard, OwlError> {
        self.try_init_with_subscriber(tracing_subscriber::registry())
    }

    /// 尝试使用调用方提供的 subscriber 初始化日志系统。
    ///
    /// 与 try_init 相同，但允许应用在初始化前组合额外 Layer。全局 subscriber 已设置时
    /// 返回 OwlError::AlreadyInitialized。
    pub fn try_init_with_subscriber<S>(self, subscriber: S) -> Result<OwlGuard, OwlError>
    where
        S: tracing::Subscriber
            + for<'a> tracing_subscriber::registry::LookupSpan<'a>
            + Send
            + Sync
            + 'static,
    {
        let config = self.config;
        validate_config(&config)?;

        // 构建环境过滤器
        let env_filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(config.level.to_string()));

        let mut console_guard: Option<tracing_appender::non_blocking::WorkerGuard> = None;
        let mut file_guard: Option<tracing_appender::non_blocking::WorkerGuard> = None;
        let mut console_dropped_lines = None;
        let mut file_dropped_lines = None;

        // 构建控制台输出层
        let console_layer = if config.enable_console {
            let (non_blocking, guard) =
                tracing_appender::non_blocking::NonBlockingBuilder::default()
                    .buffered_lines_limit(config.buffered_lines_limit)
                    .lossy(config.lossy)
                    .finish(std::io::stderr());
            console_dropped_lines = Some(non_blocking.error_counter());
            console_guard = Some(guard);

            let layer = match config.format {
                OutputFormat::Json => {
                    let json_fmt = formatter::OwlJsonFormatter {
                        show_target: config.show_target,
                        show_thread: config.show_thread,
                        show_line_number: config.show_line_number,
                        time_format: config.time_format.clone(),
                        use_utc: config.use_utc,
                        global_fields: config.global_fields.clone(),
                    };
                    tracing_subscriber::fmt::layer()
                        .with_writer(non_blocking)
                        .event_format(json_fmt)
                        .boxed()
                }
                OutputFormat::Compact => {
                    let fmt = formatter::console_compact_formatter(config.language, &config);
                    tracing_subscriber::fmt::layer()
                        .with_writer(non_blocking)
                        .event_format(fmt)
                        .with_ansi(config.enable_ansi)
                        .boxed()
                }
                OutputFormat::Pretty => {
                    let fmt = formatter::console_formatter(config.language, &config);
                    tracing_subscriber::fmt::layer()
                        .with_writer(non_blocking)
                        .event_format(fmt)
                        .with_ansi(config.enable_ansi)
                        .boxed()
                }
            };
            Some(layer)
        } else {
            None
        };

        let mut error_file_guard: Option<tracing_appender::non_blocking::WorkerGuard> = None;
        let mut error_file_dropped_lines = None;

        // 若启用文件输出或分级文件输出，先确保目录存在并清理一次过期日志
        if config.enable_file || config.error_file_level.is_some() {
            std::fs::create_dir_all(&config.log_dir).map_err(OwlError::LogDirCreation)?;
            if let Some(retention_days) = config.retention_days {
                cleanup_old_logs(
                    std::path::Path::new(&config.log_dir),
                    &config.file_name,
                    retention_days,
                );
            }
        }

        // 构建主文件输出层
        let file_layer = if config.enable_file {
            let file_writer = create_file_writer(&config, &config.file_name)?;
            let (non_blocking, guard) =
                tracing_appender::non_blocking::NonBlockingBuilder::default()
                    .buffered_lines_limit(config.buffered_lines_limit)
                    .lossy(config.lossy)
                    .finish(file_writer);
            file_dropped_lines = Some(non_blocking.error_counter());
            file_guard = Some(guard);
            Some(build_file_fmt_layer(&config, non_blocking))
        } else {
            None
        };

        // 构建按级别分离的独立文件层（如 error.log）
        let error_file_layer = if let Some(min_level) = config.error_file_level {
            let prefix = format!("{}.{}", config.file_name, min_level);
            let writer = create_file_writer(&config, &prefix)?;
            let (non_blocking, guard) =
                tracing_appender::non_blocking::NonBlockingBuilder::default()
                    .buffered_lines_limit(config.buffered_lines_limit)
                    .lossy(config.lossy)
                    .finish(writer);
            error_file_dropped_lines = Some(non_blocking.error_counter());
            error_file_guard = Some(guard);
            // 仅放行达到或严重于阈值的日志（LevelFilter 语义：WARN 放行 WARN+ERROR）
            let level_filter =
                tracing_subscriber::filter::LevelFilter::from_level(min_level.to_tracing_level());
            Some(build_file_fmt_layer(&config, non_blocking).with_filter(level_filter))
        } else {
            None
        };

        // 构建 OTLP 导出层（仅在启用 `otlp` feature 且设置了端点时生效）
        #[cfg(feature = "otlp")]
        let (otel_layer, _otel_provider): (
            Option<Box<dyn tracing_subscriber::Layer<_> + Send + Sync>>,
            Option<opentelemetry_sdk::trace::SdkTracerProvider>,
        ) = match build_otel_provider(&config)? {
            Some(provider) => {
                use opentelemetry::trace::TracerProvider as _;
                let tracer = provider.tracer("owl-logger");
                let layer: Box<dyn tracing_subscriber::Layer<_> + Send + Sync> =
                    tracing_opentelemetry::layer().with_tracer(tracer).boxed();
                (Some(layer), Some(provider))
            }
            None => (None, None),
        };
        #[cfg(not(feature = "otlp"))]
        let otel_layer: Option<Box<dyn tracing_subscriber::Layer<_> + Send + Sync>> = None;

        // 包装 reloadable filter 层
        let (env_filter_layer, reload_handle) = tracing_subscriber::reload::Layer::new(env_filter);

        // 初始化全局注册表
        // OwlSpanLayer 负责把 span 字段以结构化形式存入 extensions，供格式化器直接读取
        subscriber
            .with(env_filter_layer)
            .with(formatter::OwlSpanLayer)
            .with(otel_layer)
            .with(console_layer)
            .with(file_layer)
            .with(error_file_layer)
            .try_init()
            .map_err(|_| OwlError::AlreadyInitialized)?;
        RELOAD_HANDLE
            .set(Box::new(reload_handle))
            .map_err(|_| OwlError::AlreadyInitialized)?;

        // 桥接 log crate
        tracing_log::LogTracer::init().ok();

        // 注册 Panic 捕获钩子，增加堆栈输出
        if config.catch_panic {
            let default_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |panic_info| {
                let location = panic_info
                    .location()
                    .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
                    .unwrap_or_else(|| "unknown".to_string());

                let payload = panic_info.payload();
                let message = if let Some(s) = payload.downcast_ref::<&str>() {
                    *s
                } else if let Some(s) = payload.downcast_ref::<String>() {
                    s.as_str()
                } else {
                    "Box<dyn Any>"
                };

                let backtrace = std::backtrace::Backtrace::capture();
                let backtrace_str = format!("{}", backtrace);

                if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
                    tracing::error!(
                        target: "panic",
                        location = %location,
                        backtrace = %backtrace_str,
                        "Application panicked: {}\nBacktrace:\n{}",
                        message,
                        backtrace_str
                    );
                } else {
                    tracing::error!(
                        target: "panic",
                        location = %location,
                        "Application panicked: {}",
                        message
                    );
                }

                default_hook(panic_info);
            }));
        }

        // 启动后台过期日志周期清理任务（每小时扫描一次），由 Guard 在关闭时停止。
        let cleanup_worker = if config.enable_file || config.error_file_level.is_some() {
            config.retention_days.map(|retention_days| {
                CleanupWorker::start(
                    std::path::PathBuf::from(&config.log_dir),
                    config.file_name.clone(),
                    retention_days,
                )
            })
        } else {
            None
        };

        // 设置全局语言状态供 #[monitor] 宏查询
        crate::__private::set_language(config.language);

        // 打印初始化成功消息
        tracing::info!("{}", I18n::init_message(config.language));

        Ok(OwlGuard {
            _cleanup_worker: cleanup_worker,
            _file_guard: file_guard,
            _console_guard: console_guard,
            _error_file_guard: error_file_guard,
            _file_dropped_lines: file_dropped_lines,
            _console_dropped_lines: console_dropped_lines,
            _error_file_dropped_lines: error_file_dropped_lines,
            #[cfg(feature = "otlp")]
            _otel_provider,
            language: config.language,
        })
    }
}

/// 根据配置构建 OTLP 追踪 provider（OTLP/HTTP + 阻塞式 reqwest，无需 Tokio 运行时）
#[cfg(feature = "otlp")]
fn build_otel_provider(
    config: &OwlConfig,
) -> Result<Option<opentelemetry_sdk::trace::SdkTracerProvider>, OwlError> {
    use opentelemetry_otlp::WithExportConfig;

    let endpoint = match &config.otlp_endpoint {
        Some(endpoint) => endpoint.clone(),
        None => return Ok(None),
    };

    let service_name = config
        .otlp_service_name
        .clone()
        .unwrap_or_else(|| config.file_name.clone());

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()
        .map_err(|e| OwlError::Other(format!("OTLP exporter build failed: {e}")))?;

    let resource = opentelemetry_sdk::Resource::builder()
        .with_service_name(service_name)
        .build();

    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build();

    // 注册为全局 provider，便于跨库追踪上下文传播
    opentelemetry::global::set_tracer_provider(provider.clone());

    Ok(Some(provider))
}

/// 根据轮转策略为指定文件名前缀创建文件写入器
fn create_file_writer(
    config: &OwlConfig,
    file_name: &str,
) -> Result<Box<dyn std::io::Write + Send + Sync + 'static>, OwlError> {
    Ok(Box::new(OwlRollingFileWriter::new(
        &config.log_dir,
        file_name,
        config.rotation.clone(),
        config.max_files,
        config.retention_days,
    )))
}

/// 根据输出格式为文件写入器构建对应的格式化层（文件输出始终禁用 ANSI）
///
/// 对 `S` 泛型化，以便该层可被叠加到注册表栈的任意层级（与内联 `.boxed()` 行为一致）。
fn build_file_fmt_layer<S>(
    config: &OwlConfig,
    non_blocking: tracing_appender::non_blocking::NonBlocking,
) -> Box<dyn tracing_subscriber::Layer<S> + Send + Sync>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    match config.format {
        OutputFormat::Json => {
            let json_fmt = formatter::OwlJsonFormatter {
                show_target: config.show_target,
                show_thread: config.show_thread,
                show_line_number: config.show_line_number,
                time_format: config.time_format.clone(),
                use_utc: config.use_utc,
                global_fields: config.global_fields.clone(),
            };
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .event_format(json_fmt)
                .boxed()
        }
        OutputFormat::Compact => {
            let fmt = formatter::file_compact_formatter(config.language, config);
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .event_format(fmt)
                .with_ansi(false)
                .boxed()
        }
        OutputFormat::Pretty => {
            let fmt = formatter::file_formatter(config.language, config);
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .event_format(fmt)
                .with_ansi(false)
                .boxed()
        }
    }
}

impl Default for OwlLoggerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_log_level(value: &str) -> Result<LogLevel, OwlError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "trace" => Ok(LogLevel::Trace),
        "debug" => Ok(LogLevel::Debug),
        "info" => Ok(LogLevel::Info),
        "warn" | "warning" => Ok(LogLevel::Warn),
        "error" => Ok(LogLevel::Error),
        other => Err(OwlError::Other(format!("invalid OWL_LOG_LEVEL: {other}"))),
    }
}

fn parse_output_format(value: &str) -> Result<OutputFormat, OwlError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "pretty" => Ok(OutputFormat::Pretty),
        "compact" => Ok(OutputFormat::Compact),
        "json" => Ok(OutputFormat::Json),
        other => Err(OwlError::Other(format!("invalid OWL_LOG_FORMAT: {other}"))),
    }
}

/// 校验日志文件名前缀，确保它始终位于日志目录下。
///
/// file_name 是前缀而非路径，因此只接受单个普通路径组件。额外检查 Windows
/// 路径分隔符与盘符形式，使在 Unix 上读取环境变量时也不会放行跨平台危险值。
fn validate_file_name(file_name: &str) -> Result<(), OwlError> {
    let mut components = std::path::Path::new(file_name).components();
    let is_single_normal_component =
        matches!(components.next(), Some(std::path::Component::Normal(_)))
            && components.next().is_none();
    let has_windows_drive_prefix = file_name.len() >= 2
        && file_name.as_bytes()[0].is_ascii_alphabetic()
        && file_name.as_bytes()[1] == b':';

    if file_name.is_empty()
        || file_name.contains(['/', '\\', '\0'])
        || has_windows_drive_prefix
        || !is_single_normal_component
    {
        return Err(OwlError::Other(
            "invalid log file name: use one non-empty file-name component without path separators"
                .to_string(),
        ));
    }

    Ok(())
}

fn validate_config(config: &OwlConfig) -> Result<(), OwlError> {
    validate_file_name(&config.file_name)?;

    if config.buffered_lines_limit == 0 {
        return Err(OwlError::Other(
            "invalid buffered lines limit: it must be greater than zero".to_string(),
        ));
    }

    if let RotationPolicy::SizeMB(megabytes) = &config.rotation {
        if *megabytes == 0 {
            return Err(OwlError::Other(
                "invalid size rotation: SizeMB must be greater than zero".to_string(),
            ));
        }
        if megabytes.checked_mul(1024 * 1024).is_none() {
            return Err(OwlError::Other(
                "invalid size rotation: SizeMB is too large".to_string(),
            ));
        }
    }

    Ok(())
}

/// 支持按时间（每天、每小时）或大小限制自动轮转，且活跃日志文件名不含时间戳的自定义文件写入器
struct OwlRollingFileWriter {
    log_dir: std::path::PathBuf,
    file_name: String,
    rotation: RotationPolicy,
    max_size: u64,
    max_files: Option<usize>,
    retention_days: Option<usize>,
    current_file: Option<std::fs::File>,
    current_size: u64,
    active_date: Option<chrono::NaiveDate>,
    active_hour: Option<(chrono::NaiveDate, u32)>,
}

impl OwlRollingFileWriter {
    pub fn new(
        log_dir: impl Into<std::path::PathBuf>,
        file_name: impl Into<String>,
        rotation: RotationPolicy,
        max_files: Option<usize>,
        retention_days: Option<usize>,
    ) -> Self {
        let max_size = match &rotation {
            RotationPolicy::SizeMB(mb) => mb.saturating_mul(1024 * 1024),
            _ => 0,
        };
        Self {
            log_dir: log_dir.into(),
            file_name: file_name.into(),
            rotation,
            max_size,
            // 与 tracing-appender 的 max_log_files 保持一致：0 表示关闭数量上限。
            max_files: max_files.filter(|&max_files| max_files > 0),
            retention_days,
            current_file: None,
            current_size: 0,
            active_date: None,
            active_hour: None,
        }
    }

    fn init_file(&mut self) -> std::io::Result<&mut std::fs::File> {
        if self.current_file.is_none() {
            let file_path = self.log_dir.join(format!("{}.log", self.file_name));

            // 检查启动时是否需要轮转已经存在的老日志文件
            if file_path.exists() {
                let now = chrono::Local::now();
                let mut rotate_needed = false;
                let mut rotation_date = None;
                let mut rotation_hour = None;

                if let Ok(metadata) = file_path.metadata() {
                    if let Ok(modified) = metadata.modified() {
                        let modified_local: chrono::DateTime<chrono::Local> = modified.into();
                        match &self.rotation {
                            RotationPolicy::Daily => {
                                if modified_local.date_naive() != now.date_naive() {
                                    rotate_needed = true;
                                    rotation_date = Some(modified_local.date_naive());
                                }
                            }
                            RotationPolicy::Hourly => {
                                let mod_hour = (modified_local.date_naive(), modified_local.hour());
                                let now_hour = (now.date_naive(), now.hour());
                                if mod_hour != now_hour {
                                    rotate_needed = true;
                                    rotation_hour = Some(mod_hour);
                                }
                            }
                            RotationPolicy::SizeMB(_) => {
                                if metadata.len() >= self.max_size {
                                    rotate_needed = true;
                                }
                            }
                            RotationPolicy::Never => {}
                        }
                    }
                }

                if rotate_needed {
                    self.rotate_existing(&file_path, rotation_date, rotation_hour)?;
                }
            }

            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file_path)?;

            let metadata = file.metadata()?;
            self.current_size = metadata.len();

            let now = chrono::Local::now();
            self.active_date = Some(now.date_naive());
            self.active_hour = Some((now.date_naive(), now.hour()));
            self.current_file = Some(file);
            self.enforce_max_files()?;
        }

        self.current_file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("owl-logger: rolling file was not initialized"))
    }

    fn rotate_existing(
        &mut self,
        file_path: &std::path::Path,
        rotation_date: Option<chrono::NaiveDate>,
        rotation_hour: Option<(chrono::NaiveDate, u32)>,
    ) -> std::io::Result<()> {
        match &self.rotation {
            RotationPolicy::SizeMB(_) => {
                self.rotate_size(file_path)?;
            }
            RotationPolicy::Daily | RotationPolicy::Hourly => {
                let staging_path = unique_staging_path(&self.log_dir, &self.file_name);
                std::fs::rename(file_path, &staging_path)?;

                let date_str = match self.rotation {
                    RotationPolicy::Daily => {
                        let date =
                            rotation_date.unwrap_or_else(|| chrono::Local::now().date_naive());
                        date.format("%Y-%m-%d").to_string()
                    }
                    RotationPolicy::Hourly => {
                        let (date, hour) = rotation_hour.unwrap_or_else(|| {
                            let now = chrono::Local::now();
                            (now.date_naive(), now.hour())
                        });
                        format!("{}-{hour:02}", date.format("%Y-%m-%d"))
                    }
                    _ => unreachable!("only time-based rotation reaches this branch"),
                };

                let mut dest_path = self
                    .log_dir
                    .join(format!("{}.{}.log", self.file_name, date_str));
                if dest_path.exists() {
                    let mut index = 1;
                    loop {
                        let candidate = self
                            .log_dir
                            .join(format!("{}.{}.{}.log", self.file_name, date_str, index));
                        if !candidate.exists() {
                            dest_path = candidate;
                            break;
                        }
                        index += 1;
                    }
                }

                if let Err(error) = std::fs::rename(&staging_path, &dest_path) {
                    let _ = std::fs::rename(&staging_path, file_path);
                    return Err(error);
                }
            }
            RotationPolicy::Never => {}
        }
        Ok(())
    }

    fn rotate_size(&mut self, file_path: &std::path::Path) -> std::io::Result<()> {
        let staging_path = unique_staging_path(&self.log_dir, &self.file_name);
        std::fs::rename(file_path, &staging_path)?;

        if let Some(max_files) = self.max_files {
            if max_files > 1 {
                let oldest_index = max_files - 1;
                remove_file_if_exists(
                    &self
                        .log_dir
                        .join(format!("{}.{}.log.gz", self.file_name, oldest_index)),
                )?;
                remove_file_if_exists(
                    &self
                        .log_dir
                        .join(format!("{}.{}.log", self.file_name, oldest_index)),
                )?;

                for index in (1..oldest_index).rev() {
                    let source_gz = self
                        .log_dir
                        .join(format!("{}.{}.log.gz", self.file_name, index));
                    let destination_gz =
                        self.log_dir
                            .join(format!("{}.{}.log.gz", self.file_name, index + 1));
                    if source_gz.exists() {
                        std::fs::rename(source_gz, destination_gz)?;
                    }

                    let source_log = self
                        .log_dir
                        .join(format!("{}.{}.log", self.file_name, index));
                    let destination_log =
                        self.log_dir
                            .join(format!("{}.{}.log", self.file_name, index + 1));
                    if source_log.exists() {
                        std::fs::rename(source_log, destination_log)?;
                    }
                }

                let destination_gz = self.log_dir.join(format!("{}.1.log.gz", self.file_name));
                compress_file(&staging_path, &destination_gz)?;
            } else {
                std::fs::remove_file(staging_path)?;
            }
        } else {
            let mut index = 1;
            let backup_log = loop {
                let backup_gz_path = self
                    .log_dir
                    .join(format!("{}.{}.log.gz", self.file_name, index));
                let backup_log_path = self
                    .log_dir
                    .join(format!("{}.{}.log", self.file_name, index));
                if !backup_gz_path.exists() && !backup_log_path.exists() {
                    break backup_log_path;
                }
                index += 1;
            };
            let backup_gz = self
                .log_dir
                .join(format!("{}.{}.log.gz", self.file_name, index));
            std::fs::rename(&staging_path, &backup_log)?;
            compress_file(&backup_log, &backup_gz)?;
        }

        Ok(())
    }

    fn rotate(&mut self, now: chrono::DateTime<chrono::Local>) -> std::io::Result<()> {
        self.current_file = None;
        let file_path = self.log_dir.join(format!("{}.log", self.file_name));

        if file_path.exists() {
            let rotation_date = self.active_date;
            let rotation_hour = self.active_hour;
            self.rotate_existing(&file_path, rotation_date, rotation_hour)?;
        }

        if let Some(retention_days) = self.retention_days {
            cleanup_old_logs(&self.log_dir, &self.file_name, retention_days);
        }

        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&file_path)?;

        self.current_size = 0;
        self.active_date = Some(now.date_naive());
        self.active_hour = Some((now.date_naive(), now.hour()));
        self.current_file = Some(file);
        self.enforce_max_files()?;

        Ok(())
    }

    /// 将当前前缀的日志文件数限制在 max_files 以内。
    ///
    /// 活跃的 `{file_name}.log` 始终保留，因此最多只保留 `max_files - 1` 个历史文件。
    /// 这与 tracing-appender 的 max_log_files 语义一致，并适用于大小、按日和按小时轮转。
    fn enforce_max_files(&self) -> std::io::Result<()> {
        if let Some(max_files) = self.max_files {
            prune_excess_historical_logs(&self.log_dir, &self.file_name, max_files)?;
        }
        Ok(())
    }
}

impl std::io::Write for OwlRollingFileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let now = chrono::Local::now();
        self.init_file()?;

        let mut rotate_needed = false;
        match &self.rotation {
            RotationPolicy::Daily => {
                if let Some(active_date) = self.active_date {
                    if now.date_naive() != active_date {
                        rotate_needed = true;
                    }
                }
            }
            RotationPolicy::Hourly => {
                if let Some((active_date, active_hour)) = self.active_hour {
                    if now.date_naive() != active_date || now.hour() != active_hour {
                        rotate_needed = true;
                    }
                }
            }
            RotationPolicy::SizeMB(_) => {
                if self.current_size + buf.len() as u64 > self.max_size {
                    rotate_needed = true;
                }
            }
            RotationPolicy::Never => {}
        }

        if rotate_needed {
            self.rotate(now)?;
        }

        let file = self.current_file.as_mut().unwrap();
        let written = file.write(buf)?;
        self.current_size += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(ref mut file) = self.current_file {
            file.flush()?;
        }
        Ok(())
    }
}

fn unique_staging_path(log_dir: &std::path::Path, file_name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let thread_id = format!("{:?}", std::thread::current().id());
    log_dir.join(format!(
        "{}.rotate-staging.{}.{}.log",
        file_name, nanos, thread_id
    ))
}

fn compress_file(src_path: &std::path::Path, dest_path: &std::path::Path) -> std::io::Result<()> {
    let tmp_path = unique_temp_gzip_path(dest_path);
    let result = (|| -> std::io::Result<()> {
        let src = std::fs::File::open(src_path)?;
        let dest = std::fs::File::create(&tmp_path)?;
        let mut encoder = flate2::write::GzEncoder::new(dest, flate2::Compression::default());
        let mut reader = std::io::BufReader::new(src);
        std::io::copy(&mut reader, &mut encoder)?;
        encoder.finish()?;
        if dest_path.exists() {
            std::fs::remove_file(dest_path)?;
        }
        std::fs::rename(&tmp_path, dest_path)?;
        Ok(())
    })();

    if result.is_ok() {
        std::fs::remove_file(src_path)?;
    } else {
        let _ = std::fs::remove_file(tmp_path);
    }

    result
}

fn remove_file_if_exists(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn unique_temp_gzip_path(dest_path: &std::path::Path) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let thread_id = format!("{:?}", std::thread::current().id());
    let file_name = dest_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("owl-log.gz");
    dest_path.with_file_name(format!("{file_name}.{nanos}.{thread_id}.tmp"))
}

fn is_daily_date_str(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

fn is_hourly_date_str(s: &str) -> bool {
    let Some((date, hour)) = s.rsplit_once('-') else {
        return false;
    };

    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok()
        && hour
            .parse::<u32>()
            .is_ok_and(|hour| (0..=23).contains(&hour))
}

/// 兼容旧版时间轮转文件名：`{file}.log.YYYY-MM-DD` 或
/// `{file}.log.YYYY-MM-DD-HH`。
///
/// 不做宽松的前缀匹配，避免把用户自行创建的 `{file}.log.backup` 等文件当成历史
/// 日志删除。对无法确认归属的文件宁可保留，由用户自行处理。
fn matches_legacy_time_rotation_suffix(suffix: &str) -> bool {
    is_daily_date_str(suffix) || is_hourly_date_str(suffix)
}

fn matches_suffix(mut s: &str) -> bool {
    if s == ".log" || s == ".log.gz" {
        return true;
    }
    if s.starts_with('.') {
        s = &s[1..];
        if let Some(next_dot) = s.find('.') {
            let index_part = &s[..next_dot];
            if index_part.chars().all(|c| c.is_ascii_digit()) && !index_part.is_empty() {
                let rest = &s[next_dot..];
                if rest == ".log" || rest == ".log.gz" {
                    return true;
                }
            }
        }
    }
    false
}

/// 精确判定文件名是否是由特定前缀的日志组件生成的（包括其轮转产生的备份文件，无递归）
fn is_log_file_for_prefix_non_recursive(filename: &str, file_name: &str) -> bool {
    // 1. 完全匹配 {file_name}.log 或 {file_name}.log.gz
    if filename == format!("{}.log", file_name) || filename == format!("{}.log.gz", file_name) {
        return true;
    }

    // 2. 匹配旧格式 {file_name}.log.YYYY-MM-DD 或
    // {file_name}.log.YYYY-MM-DD-HH（Daily/Hourly 轮转文件）。
    let log_dot = format!("{}.log.", file_name);
    if filename
        .strip_prefix(&log_dot)
        .is_some_and(matches_legacy_time_rotation_suffix)
    {
        return true;
    }

    // 3. 匹配新格式 {file_name}.YYYY-MM-DD.log / .log.gz
    // 或者是 {file_name}.YYYY-MM-DD-HH.log / .log.gz
    // 或者是 {file_name}.index.log / .log.gz (Size 轮转文件)
    // 或者是带有重复序号的 {file_name}.YYYY-MM-DD.index.log / .log.gz
    let dot_prefix = format!("{}.", file_name);
    if filename.starts_with(&dot_prefix) {
        let remaining = &filename[dot_prefix.len()..];
        if let Some(first_dot) = remaining.find('.') {
            let part = &remaining[..first_dot];
            let suffix = &remaining[first_dot..];
            if matches_suffix(suffix) {
                // A. 如果 part 是纯数字 (Size 轮转索引)
                if part.chars().all(|c| c.is_ascii_digit()) && !part.is_empty() {
                    return true;
                }
                // B. 如果 part 是 YYYY-MM-DD 日期
                if is_daily_date_str(part) {
                    return true;
                }
                // C. 如果 part 是 YYYY-MM-DD-HH 级别的日期
                if is_hourly_date_str(part) {
                    return true;
                }
            }
        }
    }

    false
}

fn is_log_file_for_prefix(filename: &str, file_name: &str) -> bool {
    if is_log_file_for_prefix_non_recursive(filename, file_name) {
        return true;
    }

    // 检查各日志级别的独立文件分支（如 app.error.log 等）
    for level in &["error", "warn", "info", "debug", "trace"] {
        let level_prefix = format!("{}.{}", file_name, level);
        if is_log_file_for_prefix_non_recursive(filename, &level_prefix) {
            return true;
        }
    }

    false
}

/// 在不触及活跃日志的前提下，删除超过数量上限的最早历史日志。
///
/// 仅处理 `is_log_file_for_prefix_non_recursive` 能明确识别的文件，避免误删用户在同一
/// 目录中维护的其他文件。修改时间不可读的文件会被保留，优先保证数据安全。
fn prune_excess_historical_logs(
    log_dir: &std::path::Path,
    file_name: &str,
    max_files: usize,
) -> std::io::Result<()> {
    let max_historical_files = max_files.saturating_sub(1);
    let active_file = format!("{file_name}.log");
    let mut historical_files = Vec::new();

    for entry in std::fs::read_dir(log_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if filename == active_file || !is_log_file_for_prefix_non_recursive(filename, file_name) {
            continue;
        }

        let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
            continue;
        };
        historical_files.push((modified, path));
    }

    historical_files.sort_by(|(left_time, left_path), (right_time, right_path)| {
        left_time
            .cmp(right_time)
            .then_with(|| left_path.cmp(right_path))
    });

    let excess = historical_files.len().saturating_sub(max_historical_files);
    for (_, path) in historical_files.into_iter().take(excess) {
        std::fs::remove_file(path)?;
    }

    Ok(())
}

/// 清理过期日志文件
fn cleanup_old_logs(log_dir: &std::path::Path, file_name: &str, retention_days: usize) {
    let now = std::time::SystemTime::now();
    let max_age = std::time::Duration::from_secs(retention_days as u64 * 24 * 60 * 60);

    // 当前活跃写入的文件，绝对不被清理
    let log_exact = format!("{}.log", file_name);
    let active_error_log_prefixes = [
        format!("{}.error.log", file_name),
        format!("{}.warn.log", file_name),
        format!("{}.info.log", file_name),
        format!("{}.debug.log", file_name),
        format!("{}.trace.log", file_name),
    ];

    if let Ok(entries) = std::fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(filename_str) = path.file_name().and_then(|s| s.to_str()) {
                    // 1. 判断是否属于此日志文件的模式
                    if is_log_file_for_prefix(filename_str, file_name) {
                        // 2. 排除正在活跃写入的文件
                        let is_active = filename_str == log_exact
                            || active_error_log_prefixes.iter().any(|p| filename_str == p);

                        if !is_active {
                            if let Ok(metadata) = entry.metadata() {
                                if let Ok(modified) = metadata.modified() {
                                    if let Ok(age) = now.duration_since(modified) {
                                        if age > max_age {
                                            let _ = std::fs::remove_file(path);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_log_level_values() {
        assert_eq!(parse_log_level("TRACE").unwrap(), LogLevel::Trace);
        assert_eq!(parse_log_level("warning").unwrap(), LogLevel::Warn);
        assert!(parse_log_level("verbose").is_err());
    }

    #[test]
    fn parses_env_output_format_values() {
        assert_eq!(parse_output_format("json").unwrap(), OutputFormat::Json);
        assert_eq!(
            parse_output_format("COMPACT").unwrap(),
            OutputFormat::Compact
        );
        assert!(parse_output_format("yaml").is_err());
    }

    #[test]
    fn accepts_only_a_single_safe_log_file_name_component() {
        for valid in ["app", "service.v1", ".hidden", "服务"] {
            assert!(
                validate_file_name(valid).is_ok(),
                "expected {valid:?} to be accepted"
            );
        }

        for invalid in [
            "",
            ".",
            "..",
            "../app",
            "nested/app",
            "/tmp/app",
            "\\server\\app",
            "C:\\logs\\app",
            "C:app",
        ] {
            assert!(
                validate_file_name(invalid).is_err(),
                "expected {invalid:?} to be rejected"
            );
        }
    }

    #[test]
    fn cleanup_matches_only_known_log_rotation_file_names() {
        for filename in [
            "app.1.log",
            "app.1.log.gz",
            "app.2026-07-21.log",
            "app.2026-07-21-18.log.gz",
            "app.2026-07-21.1.log",
            "app.log.2026-07-21",
            "app.log.2026-07-21-18",
            "app.error.1.log",
        ] {
            assert!(
                is_log_file_for_prefix(filename, "app"),
                "expected {filename:?} to be recognized as an owl-logger file"
            );
        }

        for filename in [
            "app.helper.log",
            "app.log.backup",
            "app.log.2026-99-99",
            "app.log.2026-07-21.backup",
            "app.2026-99-99.log",
            "app.2026-07-21-42.log",
        ] {
            assert!(
                !is_log_file_for_prefix(filename, "app"),
                "expected {filename:?} to be preserved"
            );
        }
    }

    #[test]
    fn max_files_limits_time_rotated_logs_without_touching_the_active_file() {
        let temp_dir = std::env::temp_dir().join(format!(
            "owl-test-time-max-files-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let mut writer =
            OwlRollingFileWriter::new(&temp_dir, "app", RotationPolicy::Daily, Some(2), None);
        std::io::Write::write_all(&mut writer, b"first\n").unwrap();

        let first_date = chrono::Local::now().date_naive() - chrono::Days::new(2);
        writer.active_date = Some(first_date);
        std::io::Write::write_all(&mut writer, b"second\n").unwrap();
        let first_archive = temp_dir.join(format!("app.{}.log", first_date.format("%Y-%m-%d")));
        assert!(first_archive.exists());

        let old_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_577_836_800);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&first_archive)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old_time))
            .unwrap();

        let second_date = chrono::Local::now().date_naive() - chrono::Days::new(1);
        writer.active_date = Some(second_date);
        std::io::Write::write_all(&mut writer, b"third\n").unwrap();
        drop(writer);

        let second_archive = temp_dir.join(format!("app.{}.log", second_date.format("%Y-%m-%d")));
        assert!(!first_archive.exists(), "the oldest archive must be pruned");
        assert!(
            second_archive.exists(),
            "the most recent archive must remain"
        );
        assert_eq!(
            std::fs::read_to_string(temp_dir.join("app.log")).unwrap(),
            "third\n"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn zero_max_files_means_no_count_limit() {
        let writer = OwlRollingFileWriter::new(
            std::env::temp_dir(),
            "no-limit",
            RotationPolicy::Daily,
            Some(0),
            None,
        );

        assert_eq!(writer.max_files, None);
    }

    #[test]
    fn rejects_invalid_runtime_configuration() {
        let config = OwlConfig {
            buffered_lines_limit: 0,
            ..OwlConfig::default()
        };
        assert!(validate_config(&config).is_err());

        let config = OwlConfig {
            rotation: RotationPolicy::SizeMB(0),
            ..OwlConfig::default()
        };
        assert!(validate_config(&config).is_err());

        let config = OwlConfig {
            rotation: RotationPolicy::SizeMB(u64::MAX),
            ..OwlConfig::default()
        };
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn cleanup_worker_stops_when_dropped() {
        let worker =
            CleanupWorker::start(std::env::temp_dir(), "cleanup-worker-test".to_string(), 1);
        drop(worker);
    }

    #[test]
    fn temp_gzip_path_stays_next_to_destination() {
        let dest = std::path::Path::new("/tmp/app.1.log.gz");
        let tmp = unique_temp_gzip_path(dest);

        assert_eq!(tmp.parent(), dest.parent());
        assert!(tmp
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("app.1.log.gz.") && name.ends_with(".tmp")));
    }

    #[test]
    fn test_owl_rolling_file_writer_daily_rotation() {
        let temp_dir = std::env::temp_dir().join(format!(
            "owl-test-daily-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let file_name = "test_daily";
        let mut writer =
            OwlRollingFileWriter::new(&temp_dir, file_name, RotationPolicy::Daily, None, None);

        // First write: creates test_daily.log
        std::io::Write::write_all(&mut writer, b"hello day 1\n").unwrap();
        std::io::Write::flush(&mut writer).unwrap();

        let active_path = temp_dir.join("test_daily.log");
        assert!(active_path.exists());

        // Simulate that the active date was yesterday
        let yesterday = chrono::Local::now().date_naive() - chrono::Days::new(1);
        writer.active_date = Some(yesterday);

        // Next write triggers daily rotation
        std::io::Write::write_all(&mut writer, b"hello day 2\n").unwrap();
        std::io::Write::flush(&mut writer).unwrap();
        drop(writer);

        // The old file should be rotated to test_daily.YYYY-MM-DD.log (using the yesterday's date)
        let expected_rotated_name = format!("{}.{}.log", file_name, yesterday.format("%Y-%m-%d"));
        let rotated_path = temp_dir.join(&expected_rotated_name);
        assert!(
            rotated_path.exists(),
            "Expected rotated file to exist: {:?}",
            rotated_path
        );

        // The active file should still exist and contain the new logs
        assert!(active_path.exists());
        let active_content = std::fs::read_to_string(&active_path).unwrap();
        assert_eq!(active_content, "hello day 2\n");

        let rotated_content = std::fs::read_to_string(&rotated_path).unwrap();
        assert_eq!(rotated_content, "hello day 1\n");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_owl_rolling_file_writer_hourly_rotation() {
        let temp_dir = std::env::temp_dir().join(format!(
            "owl-test-hourly-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let file_name = "test_hourly";
        let mut writer =
            OwlRollingFileWriter::new(&temp_dir, file_name, RotationPolicy::Hourly, None, None);

        std::io::Write::write_all(&mut writer, b"hello hour 1\n").unwrap();
        std::io::Write::flush(&mut writer).unwrap();

        let active_path = temp_dir.join("test_hourly.log");
        assert!(active_path.exists());

        // Simulate that the active hour was 2 hours ago
        let now = chrono::Local::now();
        let two_hours_ago = now - chrono::Duration::hours(2);
        let active_hour_val = (two_hours_ago.date_naive(), two_hours_ago.hour());
        writer.active_hour = Some(active_hour_val);

        // Next write triggers hourly rotation
        std::io::Write::write_all(&mut writer, b"hello hour 2\n").unwrap();
        std::io::Write::flush(&mut writer).unwrap();
        drop(writer);

        let expected_rotated_name = format!(
            "{}.{}-{:02}.log",
            file_name,
            active_hour_val.0.format("%Y-%m-%d"),
            active_hour_val.1
        );
        let rotated_path = temp_dir.join(&expected_rotated_name);
        assert!(
            rotated_path.exists(),
            "Expected rotated file to exist: {:?}",
            rotated_path
        );

        assert!(active_path.exists());
        let active_content = std::fs::read_to_string(&active_path).unwrap();
        assert_eq!(active_content, "hello hour 2\n");

        let rotated_content = std::fs::read_to_string(&rotated_path).unwrap();
        assert_eq!(rotated_content, "hello hour 1\n");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn size_rotation_is_compressed_before_writer_drop_returns() {
        let temp_dir = std::env::temp_dir().join(format!(
            "owl-test-size-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let mut writer = OwlRollingFileWriter::new(
            &temp_dir,
            "test_size",
            RotationPolicy::SizeMB(1),
            None,
            None,
        );
        std::io::Write::write_all(&mut writer, b"before rotation\n").unwrap();
        std::io::Write::flush(&mut writer).unwrap();

        // 避免写入 1 MiB 测试数据，直接模拟达到大小阈值。
        writer.current_size = writer.max_size;
        std::io::Write::write_all(&mut writer, b"after rotation\n").unwrap();
        std::io::Write::flush(&mut writer).unwrap();
        drop(writer);

        let active = std::fs::read_to_string(temp_dir.join("test_size.log")).unwrap();
        assert_eq!(active, "after rotation\n");

        let rotated = temp_dir.join("test_size.1.log.gz");
        assert!(
            rotated.exists(),
            "rotated file should be compressed before drop"
        );
        let mut decoder = flate2::read::GzDecoder::new(std::fs::File::open(rotated).unwrap());
        let mut content = String::new();
        std::io::Read::read_to_string(&mut decoder, &mut content).unwrap();
        assert_eq!(content, "before rotation\n");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
