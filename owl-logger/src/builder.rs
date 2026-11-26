use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use crate::config::*;
use crate::error::OwlError;
use crate::formatter;
use crate::guard::OwlGuard;
use crate::i18n::I18n;
use crate::writer::{
    cleanup_old_logs_locked, AtomicCounter, GlobalFlushHandle, OwlAsyncWriter,
    OwlAsyncWriterOptions, OwlRollingFileWriter,
};

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
pub(crate) static RELOAD_HANDLE: OnceLock<Box<dyn ReloadableFilter>> = OnceLock::new();

/// 全局异步刷盘句柄，用于在运行期或 panic 钩子中主动排空缓冲队列。
pub(crate) static FLUSH_HANDLE: OnceLock<GlobalFlushHandle> = OnceLock::new();

/// 记录初始化时是否启用了 `catch_panic`，供运行期 `set_filter` 保护 `panic=error` 目标不被模块级规则意外屏蔽。
pub(crate) static CATCH_PANIC_ENABLED: AtomicBool = AtomicBool::new(false);

/// 周期性保留期清理任务。
///
/// 由 OwlGuard 持有；Drop 时发送停止信号并等待线程退出，避免 logger 已关闭后仍有
/// 清理线程访问日志目录。与 `OwlRollingFileWriter` 共享 `dir_lock` 消除并发删除竞态。
pub(crate) struct CleanupWorker {
    stop_sender: Option<std::sync::mpsc::Sender<()>>,
    join_handle: Option<std::thread::JoinHandle<()>>,
}

impl CleanupWorker {
    fn start(
        log_dir: PathBuf,
        file_name: String,
        retention_days: usize,
        dir_lock: Arc<Mutex<()>>,
    ) -> Self {
        let (stop_sender, stop_receiver) = std::sync::mpsc::channel();
        let join_handle = std::thread::Builder::new()
            .name("owl-logger-cleanup".to_string())
            .spawn(move || loop {
                match stop_receiver.recv_timeout(Duration::from_secs(3600)) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        cleanup_old_logs_locked(&log_dir, &file_name, retention_days, &dir_lock);
                    }
                }
            })
            .expect("owl-logger: failed to spawn cleanup worker thread");

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
    explicit_level: bool,
}

impl OwlLoggerBuilder {
    /// 创建一个使用默认配置的 Builder
    pub fn new() -> Self {
        Self {
            config: OwlConfig::default(),
            explicit_level: false,
        }
    }

    /// 从环境变量创建 Builder
    pub fn from_env() -> Result<Self, OwlError> {
        let mut builder = Self::new();

        if let Ok(level) = std::env::var("OWL_LOG_LEVEL") {
            builder = builder.level(parse_log_level(&level)?);
        }
        if let Ok(console_enable) = std::env::var("OWL_LOG_CONSOLE") {
            builder = builder.console(parse_bool_env("OWL_LOG_CONSOLE", &console_enable)?);
        }
        if let Ok(console_level) = std::env::var("OWL_LOG_CONSOLE_LEVEL") {
            if is_off_value(&console_level) {
                builder = builder.console(false);
            } else {
                builder = builder.console_level(parse_log_level(&console_level)?);
            }
        }
        if let Ok(file_enable) = std::env::var("OWL_LOG_FILE_ENABLE") {
            builder = builder.file(parse_bool_env("OWL_LOG_FILE_ENABLE", &file_enable)?);
        }
        if let Ok(file_level) = std::env::var("OWL_LOG_FILE_LEVEL") {
            if is_off_value(&file_level) {
                builder = builder.file(false);
            } else {
                builder = builder.file_level(parse_log_level(&file_level)?);
            }
        }
        if let Ok(error_file) = std::env::var("OWL_LOG_ERROR_FILE") {
            if is_off_value(&error_file) || error_file.trim().eq_ignore_ascii_case("none") {
                builder.config.error_file_level = None;
            } else {
                builder = builder.error_file(parse_log_level(&error_file)?);
            }
        }
        if let Ok(format) = std::env::var("OWL_LOG_FORMAT") {
            builder = builder.format(parse_output_format(&format)?);
        }
        if let Ok(console_fmt) = std::env::var("OWL_LOG_CONSOLE_FORMAT") {
            builder = builder.console_format(parse_output_format(&console_fmt)?);
        }
        if let Ok(file_fmt) = std::env::var("OWL_LOG_FILE_FORMAT") {
            builder = builder.file_format(parse_output_format(&file_fmt)?);
        }
        if let Ok(rotation) = std::env::var("OWL_LOG_ROTATION") {
            builder = builder.rotation(parse_rotation_policy(&rotation)?);
        }
        if let Ok(lang) = std::env::var("OWL_LOG_LANG") {
            builder = builder.language(parse_language(&lang)?);
        }
        if let Ok(utc) = std::env::var("OWL_LOG_UTC") {
            builder = builder.utc(parse_bool_env("OWL_LOG_UTC", &utc)?);
        }
        if let Ok(isolation) = std::env::var("OWL_LOG_PROCESS_ISOLATION") {
            builder =
                builder.process_isolation(parse_bool_env("OWL_LOG_PROCESS_ISOLATION", &isolation)?);
        }
        if let Ok(retention) = std::env::var("OWL_LOG_RETENTION_DAYS") {
            let days = retention.trim().parse::<usize>().map_err(|_| {
                OwlError::Other(format!("invalid OWL_LOG_RETENTION_DAYS: {retention}"))
            })?;
            builder = builder.retention_days(days);
        }
        if let Ok(max_files) = std::env::var("OWL_LOG_MAX_FILES") {
            let count = max_files
                .trim()
                .parse::<usize>()
                .map_err(|_| OwlError::Other(format!("invalid OWL_LOG_MAX_FILES: {max_files}")))?;
            builder = builder.max_files(count);
        }
        if let Ok(log_dir) = std::env::var("OWL_LOG_DIR") {
            builder = builder.log_dir(log_dir);
        }
        if let Ok(file_name) = std::env::var("OWL_LOG_FILE") {
            builder = builder.file_name(file_name);
        }
        if let Ok(endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
            let trimmed = endpoint.trim();
            if !trimmed.is_empty() {
                builder = builder.otlp_endpoint(trimmed);
            }
        }
        if let Ok(service_name) = std::env::var("OTEL_SERVICE_NAME") {
            let trimmed = service_name.trim();
            if !trimmed.is_empty() {
                builder = builder.otlp_service_name(trimmed);
            }
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

    /// 设置全局最低日志级别
    pub fn level(mut self, level: LogLevel) -> Self {
        self.config.level = level;
        self.explicit_level = true;
        self
    }

    /// 设置控制台独立最低日志级别（例如控制台只输出 `Warn` 及以上，文件保留 `Debug`）
    pub fn console_level(mut self, level: LogLevel) -> Self {
        self.config.console_level = Some(level);
        self
    }

    /// 设置主文件独立最低日志级别
    pub fn file_level(mut self, level: LogLevel) -> Self {
        self.config.file_level = Some(level);
        self
    }

    /// 设置输出语言
    pub fn language(mut self, lang: Language) -> Self {
        self.config.language = lang;
        self
    }

    /// 设置默认输出格式（同时作用于未单独指定格式的控制台和文件）
    pub fn format(mut self, fmt: OutputFormat) -> Self {
        self.config.format = fmt;
        self
    }

    /// 单独设置控制台输出格式（例如控制台使用彩色 `Pretty`，文件使用结构化 `Json`）
    pub fn console_format(mut self, fmt: OutputFormat) -> Self {
        self.config.console_format = Some(fmt);
        self
    }

    /// 单独设置文件输出格式（同时作用于主文件与 `error_file`）
    pub fn file_format(mut self, fmt: OutputFormat) -> Self {
        self.config.file_format = Some(fmt);
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

    /// 设置是否使用 UTC 时区代替本地时区（同时作用于时间戳格式化与按天/按小时轮转切分）
    pub fn utc(mut self, use_utc: bool) -> Self {
        self.config.use_utc = use_utc;
        self
    }

    /// 设置最大日志文件保留数量
    ///
    /// 上限包含当前活跃日志文件；传入 `0` 表示不限制数量。
    pub fn max_files(mut self, max_files: usize) -> Self {
        self.config.max_files = if max_files == 0 {
            None
        } else {
            Some(max_files)
        };
        self
    }

    /// 设置是否捕获 Panic 并通过日志输出。
    ///
    /// 该选项会替换进程级 panic hook，强制捕获堆栈并在退出前紧急刷盘，默认关闭。
    /// 建议只在应用程序入口显式启用，不要由可复用库开启。
    pub fn catch_panic(mut self, catch: bool) -> Self {
        self.config.catch_panic = catch;
        self
    }

    /// 添加全局属性字段
    pub fn global_field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.global_fields.insert(key.into(), value.into());
        self
    }

    /// 设置日志文件保留天数。
    ///
    /// 与 `max_files(0)` 一致，传入 `0` 表示关闭按天自动清理（永久保留）。
    pub fn retention_days(mut self, days: usize) -> Self {
        self.config.retention_days = if days == 0 { None } else { Some(days) };
        self
    }

    /// 启用按级别分离的独立日志文件。
    ///
    /// 开启后会额外写入一个 `{file_name}.{level}.log` 文件，仅记录达到或严重于
    /// `min_level` 的日志，同时完整保留常规 Span 上下文字段。
    pub fn error_file(mut self, min_level: LogLevel) -> Self {
        self.config.error_file_level = Some(min_level);
        self
    }

    /// 启用或禁用多进程文件名隔离（借鉴 Python `yyds-logger` 的 `process_isolation`）。
    ///
    /// 开启后，活跃日志与分级日志文件名会自动附带当前进程 PID（如 `app.pid12345.log`、`app.pid12345.error.log`），
    /// 避免多 worker 子进程共享同一日志目录时发生跨进程轮转截断或归档重命名冲突。
    pub fn process_isolation(mut self, enable: bool) -> Self {
        self.config.process_isolation = enable;
        self
    }

    /// 设置 OTLP 导出端点（OTLP/HTTP，支持 `http://` 与 `https://`，如 `http://localhost:4318/v1/traces`）。
    ///
    /// 需要启用 `otlp` feature 才会真正导出；未启用该 feature 时将在初始化后输出警告提示。
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

    /// 设置缓冲区写满时是否立即丢弃日志条目（`false` 时阻塞背压，可配合 `queue_timeout` 限定最长等待）
    pub fn lossy(mut self, lossy: bool) -> Self {
        self.config.lossy = lossy;
        self
    }

    /// 设置非有损模式（`lossy(false)`）下队列写满时的最长阻塞等待时间。
    ///
    /// 超时仍无空位时将降级丢弃该条日志并计入 `dropped_lines()`，避免磁盘挂死永久卡住业务线程。
    pub fn queue_timeout(mut self, timeout: Duration) -> Self {
        self.config.queue_timeout = Some(timeout);
        self
    }

    /// 设置主动 `flush()` 与停机排空等待后台线程的最大超时时间（默认 30 秒）。
    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.config.shutdown_timeout = Some(timeout);
        self
    }

    /// 取消刷盘与停机等待超时限制（无限期等待后台队列完全排空）。
    pub fn no_shutdown_timeout(mut self) -> Self {
        self.config.shutdown_timeout = None;
        self
    }

    /// 构建并初始化全局日志 subscriber
    pub fn init(self) -> OwlGuard {
        self.try_init()
            .expect("owl-logger: failed to initialize. Is the global subscriber already set?")
    }

    /// 使用调用方提供的 subscriber 构建并初始化全局日志系统。
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
    /// 与 `try_init` 相同，但允许应用在初始化前组合额外 Layer。全局 subscriber 已设置时
    /// 在产生任何磁盘/线程副作用之前快速返回 `OwlError::AlreadyInitialized`。
    pub fn try_init_with_subscriber<S>(self, subscriber: S) -> Result<OwlGuard, OwlError>
    where
        S: tracing::Subscriber
            + for<'a> tracing_subscriber::registry::LookupSpan<'a>
            + Send
            + Sync
            + 'static,
    {
        let explicit_level = self.explicit_level;
        let config = self.config;
        validate_config(&config)?;

        // 在创建目录、清理历史日志、打开文件、拉起工作线程或设置 OTLP 全局状态前先快速失败
        if tracing::dispatcher::has_been_set() {
            return Err(OwlError::AlreadyInitialized);
        }

        let effective_retention = config.retention_days.filter(|&days| days > 0);
        let dir_lock = Arc::new(Mutex::new(()));
        let effective_file_name = if config.process_isolation {
            format!("{}.pid{}", config.file_name, std::process::id())
        } else {
            config.file_name.clone()
        };

        // 计算全局基础过滤级别（若未设置 RUST_LOG，自动取各已启用通道中最详细的级别，确保独立 sink 级别生效）
        let base_level = compute_effective_base_level(&config, explicit_level);
        let env_filter = std::env::var(EnvFilter::DEFAULT_ENV)
            .ok()
            .and_then(|raw| build_normalized_env_filter(&raw, config.catch_panic).ok())
            .unwrap_or_else(|| EnvFilter::new(base_level.to_string()));

        // 若显式设置了 `.level(...)` 且另一通道（如 `file_level`）拉宽了全局 base_level，
        // 为未单独指定级别的通道补齐 `config.level` 过滤，保持符合直觉的各通道语义。
        let console_filter_level = config.console_level.or_else(|| {
            (explicit_level && base_level.verbosity_rank() > config.level.verbosity_rank())
                .then_some(config.level)
        });
        let file_filter_level = config.file_level.or_else(|| {
            (explicit_level && base_level.verbosity_rank() > config.level.verbosity_rank())
                .then_some(config.level)
        });

        // 在创建目录、清理历史日志或打开文件之前先校验并构建 OTLP 导出层，确保配置错误零磁盘副作用
        #[cfg(feature = "otlp")]
        let (otel_layer, otel_provider): (
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

        // 若启用文件输出或分级文件输出，先确保目录存在、清理过期日志，并在初始化阶段**立即打开校验文件可写性**
        if config.enable_file || config.error_file_level.is_some() {
            std::fs::create_dir_all(&config.log_dir).map_err(OwlError::LogDirCreation)?;
            if let Some(retention_days) = effective_retention {
                cleanup_old_logs_locked(
                    Path::new(&config.log_dir),
                    &effective_file_name,
                    retention_days,
                    &dir_lock,
                );
            }
        }

        let prepared_main_writer = if config.enable_file {
            let sink_errors = AtomicCounter::new();
            let writer = create_eager_file_writer(
                &config,
                &effective_file_name,
                effective_retention,
                Arc::clone(&dir_lock),
                sink_errors.clone(),
            )?;
            Some((writer, sink_errors))
        } else {
            None
        };

        let prepared_error_writer = if let Some(min_level) = config.error_file_level {
            let prefix = format!("{effective_file_name}.{min_level}");
            let sink_errors = AtomicCounter::new();
            let writer = create_eager_file_writer(
                &config,
                &prefix,
                effective_retention,
                Arc::clone(&dir_lock),
                sink_errors.clone(),
            )?;
            Some((min_level, writer, sink_errors))
        } else {
            None
        };

        let mut flush_handles = Vec::new();

        // 构建控制台输出层
        let mut console_guard = None;
        let mut console_dropped_lines = None;
        let mut console_sink_errors = None;
        let console_layer = if config.enable_console {
            let (async_writer, guard, flush_handle) = OwlAsyncWriter::spawn(
                BufWriter::with_capacity(8 * 1024, std::io::stderr()),
                OwlAsyncWriterOptions {
                    buffered_lines_limit: config.buffered_lines_limit,
                    lossy: config.lossy,
                    queue_timeout: config.queue_timeout,
                    shutdown_timeout: config.shutdown_timeout,
                    thread_name: "owl-logger-console".to_string(),
                    sink_errors: None,
                },
            );
            console_dropped_lines = Some(async_writer.dropped_counter());
            console_sink_errors = Some(async_writer.sink_error_counter());
            console_guard = Some(guard);
            flush_handles.push(flush_handle);

            let console_fmt = config.console_format.unwrap_or(config.format);
            let base_layer = build_console_fmt_layer(&config, console_fmt, async_writer);
            let layer = if let Some(min_level) = console_filter_level {
                base_layer
                    .with_filter(span_preserving_level_filter(min_level))
                    .boxed()
            } else {
                base_layer
            };
            Some(layer)
        } else {
            None
        };

        // 构建主文件输出层
        let file_fmt = config.file_format.unwrap_or(config.format);
        let mut file_guard = None;
        let mut file_dropped_lines = None;
        let mut file_sink_errors = None;
        let file_layer = if let Some((file_writer, sink_errors)) = prepared_main_writer {
            let (async_writer, guard, flush_handle) = OwlAsyncWriter::spawn(
                file_writer,
                OwlAsyncWriterOptions {
                    buffered_lines_limit: config.buffered_lines_limit,
                    lossy: config.lossy,
                    queue_timeout: config.queue_timeout,
                    shutdown_timeout: config.shutdown_timeout,
                    thread_name: "owl-logger-file".to_string(),
                    sink_errors: Some(sink_errors),
                },
            );
            file_dropped_lines = Some(async_writer.dropped_counter());
            file_sink_errors = Some(async_writer.sink_error_counter());
            file_guard = Some(guard);
            flush_handles.push(flush_handle);

            let base_layer = build_file_fmt_layer(&config, file_fmt, async_writer);
            let layer = if let Some(min_level) = file_filter_level {
                base_layer
                    .with_filter(span_preserving_level_filter(min_level))
                    .boxed()
            } else {
                base_layer
            };
            Some(layer)
        } else {
            None
        };

        // 构建按级别分离的独立文件层（如 error.log），使用保留 Span 的事件级过滤器，确保不丢失 info_span 字段
        let mut error_file_guard = None;
        let mut error_file_dropped_lines = None;
        let mut error_file_sink_errors = None;
        let error_file_layer =
            if let Some((min_level, error_writer, sink_errors)) = prepared_error_writer {
                let (async_writer, guard, flush_handle) = OwlAsyncWriter::spawn(
                    error_writer,
                    OwlAsyncWriterOptions {
                        buffered_lines_limit: config.buffered_lines_limit,
                        lossy: config.lossy,
                        queue_timeout: config.queue_timeout,
                        shutdown_timeout: config.shutdown_timeout,
                        thread_name: "owl-logger-error-file".to_string(),
                        sink_errors: Some(sink_errors),
                    },
                );
                error_file_dropped_lines = Some(async_writer.dropped_counter());
                error_file_sink_errors = Some(async_writer.sink_error_counter());
                error_file_guard = Some(guard);
                flush_handles.push(flush_handle);

                Some(
                    build_file_fmt_layer(&config, file_fmt, async_writer)
                        .with_filter(span_preserving_level_filter(min_level)),
                )
            } else {
                None
            };

        // 包装 reloadable filter 层
        let (env_filter_layer, reload_handle) = tracing_subscriber::reload::Layer::new(env_filter);

        // 初始化全局注册表
        subscriber
            .with(env_filter_layer)
            .with(formatter::OwlSpanLayer)
            .with(otel_layer)
            .with(console_layer)
            .with(file_layer)
            .with(error_file_layer)
            .with(formatter::OwlEventCleanupLayer)
            .try_init()
            .map_err(|_| OwlError::AlreadyInitialized)?;

        let global_flush = GlobalFlushHandle {
            handles: flush_handles,
            default_timeout: config.shutdown_timeout,
        };
        CATCH_PANIC_ENABLED.store(config.catch_panic, Ordering::Relaxed);
        let _ = RELOAD_HANDLE.set(Box::new(reload_handle));
        let _ = FLUSH_HANDLE.set(global_flush.clone());

        // subscriber 成功注册后再设置全局 OTLP provider，避免初始化失败污染全局状态
        #[cfg(feature = "otlp")]
        if let Some(ref provider) = otel_provider {
            opentelemetry::global::set_tracer_provider(provider.clone());
        }

        // 桥接 log crate
        tracing_log::LogTracer::init().ok();

        // 注册 Panic 捕获钩子：强制抓取堆栈、避免单行重复两遍堆栈，并在退出前同步刷盘（防 panic="abort" 丢日志）
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

                let backtrace = std::backtrace::Backtrace::force_capture();
                if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
                    let backtrace_str = format!("{backtrace}");
                    tracing::error!(
                        target: "panic",
                        location = %location,
                        backtrace = %backtrace_str,
                        "Application panicked: {}",
                        message
                    );
                } else {
                    tracing::error!(
                        target: "panic",
                        location = %location,
                        "Application panicked: {}",
                        message
                    );
                }

                // 紧急排空所有异步通道，保证即使编译为 panic = "abort" 也能将崩溃日志落盘
                let _ = crate::flush();

                default_hook(panic_info);
            }));
        }

        // 启动后台过期日志周期清理任务（每小时扫描一次），由 Guard 在关闭时停止。
        let cleanup_worker = if config.enable_file || config.error_file_level.is_some() {
            effective_retention.map(|retention_days| {
                CleanupWorker::start(
                    PathBuf::from(&config.log_dir),
                    effective_file_name,
                    retention_days,
                    dir_lock,
                )
            })
        } else {
            None
        };

        // 设置全局语言状态供 #[monitor] 宏查询
        crate::__private::set_language(config.language);

        // 打印初始化成功消息
        tracing::info!("{}", I18n::init_message(config.language));

        #[cfg(not(feature = "otlp"))]
        if config.otlp_endpoint.is_some() {
            tracing::warn!(
                "owl-logger: otlp_endpoint is configured, but the `otlp` Cargo feature is disabled; spans will not be exported"
            );
        }

        Ok(OwlGuard {
            _cleanup_worker: cleanup_worker,
            _file_guard: file_guard,
            _console_guard: console_guard,
            _error_file_guard: error_file_guard,
            _file_dropped_lines: file_dropped_lines,
            _console_dropped_lines: console_dropped_lines,
            _error_file_dropped_lines: error_file_dropped_lines,
            _file_sink_errors: file_sink_errors,
            _console_sink_errors: console_sink_errors,
            _error_file_sink_errors: error_file_sink_errors,
            _flush_handle: global_flush,
            shutdown_timeout: config.shutdown_timeout,
            #[cfg(feature = "otlp")]
            _otel_provider: otel_provider,
            language: config.language,
        })
    }
}

/// 仅过滤低于阈值的 Event，始终放行 Span 元数据，避免 per-layer 过滤剥离 `info_span!` 等调用链上下文。
fn span_preserving_level_filter(
    min_level: LogLevel,
) -> tracing_subscriber::filter::FilterFn<impl Fn(&tracing::Metadata<'_>) -> bool + Clone> {
    let min_tracing_level = min_level.to_tracing_level();
    tracing_subscriber::filter::filter_fn(move |metadata| {
        metadata.is_span() || *metadata.level() <= min_tracing_level
    })
}

fn compute_effective_base_level(config: &OwlConfig, explicit_level: bool) -> LogLevel {
    let mut candidates = Vec::with_capacity(4);
    if explicit_level || (config.console_level.is_none() && config.file_level.is_none()) {
        candidates.push(config.level);
    }
    if config.enable_console {
        candidates.push(config.console_level.unwrap_or(config.level));
    }
    if config.enable_file {
        candidates.push(config.file_level.unwrap_or(config.level));
    }
    if let Some(err_lvl) = config.error_file_level {
        candidates.push(err_lvl);
    }
    candidates
        .into_iter()
        .max_by_key(|lvl| lvl.verbosity_rank())
        .unwrap_or(config.level)
}

/// 根据配置构建 OTLP 追踪 provider（OTLP/HTTP + 阻塞式 reqwest + rustls TLS，无需 Tokio 运行时）
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

    let export_timeout = config
        .shutdown_timeout
        .unwrap_or(Duration::from_secs(10))
        .min(Duration::from_secs(10));

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .with_timeout(export_timeout)
        .build()
        .map_err(|e| OwlError::Other(format!("OTLP exporter build failed: {e}")))?;

    let resource = opentelemetry_sdk::Resource::builder()
        .with_service_name(service_name)
        .build();

    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build();

    Ok(Some(provider))
}

/// 创建文件写入器并在启动阶段立即打开校验目标文件（失败时返回 `OwlError::FileAppenderCreation`）
fn create_eager_file_writer(
    config: &OwlConfig,
    file_name: &str,
    retention_days: Option<usize>,
    dir_lock: Arc<Mutex<()>>,
    sink_errors: AtomicCounter,
) -> Result<OwlRollingFileWriter, OwlError> {
    let mut writer = OwlRollingFileWriter::with_options(
        &config.log_dir,
        file_name,
        config.rotation.clone(),
        config.max_files,
        retention_days,
        config.use_utc,
        dir_lock,
        sink_errors,
    );
    writer.init_file().map_err(|e| {
        OwlError::FileAppenderCreation(format!(
            "failed to open `{}/{}.log`: {e}",
            config.log_dir, file_name
        ))
    })?;
    Ok(writer)
}

/// 根据输出格式为控制台构建对应的格式化层
fn build_console_fmt_layer<S>(
    config: &OwlConfig,
    format: OutputFormat,
    writer: OwlAsyncWriter,
) -> Box<dyn tracing_subscriber::Layer<S> + Send + Sync>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    match format {
        OutputFormat::Json => {
            let json_fmt = formatter::json_formatter(config);
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .event_format(json_fmt)
                .boxed()
        }
        OutputFormat::Compact => {
            let fmt = formatter::console_compact_formatter(config.language, config);
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .event_format(fmt)
                .with_ansi(config.enable_ansi)
                .boxed()
        }
        OutputFormat::Pretty => {
            let fmt = formatter::console_formatter(config.language, config);
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .event_format(fmt)
                .with_ansi(config.enable_ansi)
                .boxed()
        }
    }
}

/// 根据输出格式为文件写入器构建对应的格式化层（文件输出始终禁用 ANSI）
fn build_file_fmt_layer<S>(
    config: &OwlConfig,
    format: OutputFormat,
    writer: OwlAsyncWriter,
) -> Box<dyn tracing_subscriber::Layer<S> + Send + Sync>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    match format {
        OutputFormat::Json => {
            let json_fmt = formatter::json_formatter(config);
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .event_format(json_fmt)
                .boxed()
        }
        OutputFormat::Compact => {
            let fmt = formatter::file_compact_formatter(config.language, config);
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .event_format(fmt)
                .with_ansi(false)
                .boxed()
        }
        OutputFormat::Pretty => {
            let fmt = formatter::file_formatter(config.language, config);
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
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

fn is_bare_active_level_directive(segment: &str) -> bool {
    matches!(
        segment.to_ascii_lowercase().as_str(),
        "trace" | "debug" | "info" | "warn" | "warning" | "error"
    )
}

fn directive_target_prefix(segment: &str) -> &str {
    segment.split(['=', '[']).next().unwrap_or(segment).trim()
}

/// 解析并规范化 `EnvFilter` 表达式：
///
/// 当调用方使用纯模块作用域规则（如 `my_service=info`）而未指定全局裸级别时，
/// `tracing_subscriber::EnvFilter` 会将所有未显式提及的目标默认置为 `OFF`，这会导致
/// `owl_logger::context`（请求 ID Span）和 `panic`（崩溃钩子）被静默屏蔽。
/// 此辅助函数在不改动显式全局级别规则（如 `info,monitor=warn` 或 `off`）的前提下，
/// 为纯模块规则自动补齐 `owl_logger::context=error`（以及开启 `catch_panic` 时的 `panic=error`）。
pub(crate) fn build_normalized_env_filter(
    raw: &str,
    catch_panic: bool,
) -> Result<EnvFilter, OwlError> {
    let mut filter = EnvFilter::try_new(raw).map_err(|e| OwlError::EnvFilter(e.to_string()))?;

    let segments: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if segments.is_empty() || segments.iter().all(|s| s.eq_ignore_ascii_case("off")) {
        return Ok(filter);
    }

    if !segments.iter().any(|s| is_bare_active_level_directive(s)) {
        let mentions_owl_context = segments.iter().any(|s| {
            let target = directive_target_prefix(s);
            target == "owl_logger" || target.starts_with("owl_logger::")
        });
        if !mentions_owl_context {
            if let Ok(directive) = "owl_logger::context=error".parse() {
                filter = filter.add_directive(directive);
            }
        }

        if catch_panic {
            let mentions_panic = segments.iter().any(|s| {
                let target = directive_target_prefix(s);
                target == "panic" || target.starts_with("panic::")
            });
            if !mentions_panic {
                if let Ok(directive) = "panic=error".parse() {
                    filter = filter.add_directive(directive);
                }
            }
        }
    }

    Ok(filter)
}

fn is_off_value(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "off" | "false")
}

fn parse_log_level(value: &str) -> Result<LogLevel, OwlError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "trace" => Ok(LogLevel::Trace),
        "debug" => Ok(LogLevel::Debug),
        "info" => Ok(LogLevel::Info),
        "warn" | "warning" => Ok(LogLevel::Warn),
        "error" => Ok(LogLevel::Error),
        other => Err(OwlError::Other(format!("invalid log level: {other}"))),
    }
}

fn parse_output_format(value: &str) -> Result<OutputFormat, OwlError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "pretty" => Ok(OutputFormat::Pretty),
        "compact" => Ok(OutputFormat::Compact),
        "json" => Ok(OutputFormat::Json),
        other => Err(OwlError::Other(format!("invalid output format: {other}"))),
    }
}

fn parse_rotation_policy(value: &str) -> Result<RotationPolicy, OwlError> {
    let lowered = value.trim().to_ascii_lowercase();
    match lowered.as_str() {
        "daily" => Ok(RotationPolicy::Daily),
        "hourly" => Ok(RotationPolicy::Hourly),
        "never" | "none" | "off" => Ok(RotationPolicy::Never),
        other => {
            let num_str = other
                .strip_suffix("mb")
                .or_else(|| other.strip_suffix('m'))
                .unwrap_or(other)
                .trim();
            let mb = num_str
                .parse::<u64>()
                .map_err(|_| OwlError::Other(format!("invalid OWL_LOG_ROTATION: {value}")))?;
            if mb == 0 || mb.checked_mul(1024 * 1024).is_none() {
                return Err(OwlError::Other(format!(
                    "invalid OWL_LOG_ROTATION: {value}"
                )));
            }
            Ok(RotationPolicy::SizeMB(mb))
        }
    }
}

fn parse_language(value: &str) -> Result<Language, OwlError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "en" | "english" => Ok(Language::En),
        "zh" | "cn" | "zh-cn" | "chinese" => Ok(Language::Zh),
        other => Err(OwlError::Other(format!("invalid OWL_LOG_LANG: {other}"))),
    }
}

fn parse_bool_env(name: &str, value: &str) -> Result<bool, OwlError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(OwlError::Other(format!("invalid {name}: {other}"))),
    }
}

fn is_windows_reserved_stem(file_name: &str) -> bool {
    let stem = file_name.split('.').next().unwrap_or(file_name);
    matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// 校验日志文件名前缀，确保它始终位于日志目录下且跨平台合法。
fn validate_file_name(file_name: &str) -> Result<(), OwlError> {
    let mut components = Path::new(file_name).components();
    let is_single_normal_component =
        matches!(components.next(), Some(std::path::Component::Normal(_)))
            && components.next().is_none();
    let has_windows_drive_prefix = file_name.len() >= 2
        && file_name.as_bytes()[0].is_ascii_alphabetic()
        && file_name.as_bytes()[1] == b':';

    if file_name.is_empty()
        || file_name.ends_with([' ', '.'])
        || file_name.contains(['/', '\\', '\0', '<', '>', ':', '"', '|', '?', '*'])
        || file_name.chars().any(char::is_control)
        || has_windows_drive_prefix
        || is_windows_reserved_stem(file_name)
        || !is_single_normal_component
    {
        return Err(OwlError::Other(
            "invalid log file name: use one safe non-empty file-name component".to_string(),
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

    if let Some(shutdown_timeout) = config.shutdown_timeout {
        if shutdown_timeout.is_zero() {
            return Err(OwlError::Other(
                "invalid shutdown timeout: it must be greater than zero".to_string(),
            ));
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::{
        cleanup_old_logs, is_log_file_for_prefix, unique_temp_gzip_path, OwlRollingFileWriter,
    };
    use chrono::Timelike;

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
    fn parses_language_and_bool_env_values() {
        assert_eq!(parse_language("ZH").unwrap(), Language::Zh);
        assert_eq!(parse_language("en").unwrap(), Language::En);
        assert!(parse_language("fr").is_err());

        assert!(parse_bool_env("OWL_LOG_UTC", "true").unwrap());
        assert!(!parse_bool_env("OWL_LOG_CONSOLE", "off").unwrap());
        assert!(parse_bool_env("OWL_LOG_UTC", "maybe").is_err());
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
            "app.",
            "app ",
            "CON",
            "nul.v1",
            "LPT1",
            "bad:name",
            "bad*name",
            "bad\nname",
        ] {
            assert!(
                validate_file_name(invalid).is_err(),
                "expected {invalid:?} to be rejected"
            );
        }
    }

    #[test]
    fn parses_rotation_policy_from_env_strings() {
        assert_eq!(
            parse_rotation_policy("Daily").unwrap(),
            RotationPolicy::Daily
        );
        assert_eq!(
            parse_rotation_policy("HOURLY").unwrap(),
            RotationPolicy::Hourly
        );
        assert_eq!(
            parse_rotation_policy("never").unwrap(),
            RotationPolicy::Never
        );
        assert_eq!(parse_rotation_policy("off").unwrap(), RotationPolicy::Never);
        assert_eq!(
            parse_rotation_policy("50").unwrap(),
            RotationPolicy::SizeMB(50)
        );
        assert_eq!(
            parse_rotation_policy("128MB").unwrap(),
            RotationPolicy::SizeMB(128)
        );
        assert_eq!(
            parse_rotation_policy("64 m").unwrap(),
            RotationPolicy::SizeMB(64)
        );
        assert!(parse_rotation_policy("0mb").is_err());
        assert!(parse_rotation_policy("weekly").is_err());
    }

    #[test]
    fn normalized_env_filter_preserves_context_and_panic_for_module_scoped_directives() {
        let scoped = build_normalized_env_filter("my_service=info", true)
            .unwrap()
            .to_string();
        assert!(scoped.contains("my_service=info"), "got: {scoped}");
        assert!(
            scoped.contains("owl_logger::context=error"),
            "module-scoped filter must preserve request context spans: {scoped}"
        );
        assert!(
            scoped.contains("panic=error"),
            "module-scoped filter with catch_panic must preserve panic target: {scoped}"
        );

        let global = build_normalized_env_filter("info,monitor=warn", true)
            .unwrap()
            .to_string();
        assert!(
            !global.contains("owl_logger::context"),
            "bare global level must remain clean without extra directives: {global}"
        );

        let muted = build_normalized_env_filter("off", true)
            .unwrap()
            .to_string();
        assert_eq!(muted, "off");

        let explicit_off = build_normalized_env_filter("owl_logger=off,my_service=info", false)
            .unwrap()
            .to_string();
        assert!(
            !explicit_off.contains("owl_logger::context=error"),
            "explicit owl_logger override must be respected: {explicit_off}"
        );
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

        let old_time = std::time::UNIX_EPOCH + Duration::from_secs(1_577_836_800);
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
    fn zero_max_files_and_zero_retention_days_mean_no_limit() {
        let writer = OwlRollingFileWriter::new(
            std::env::temp_dir(),
            "no-limit",
            RotationPolicy::Daily,
            Some(0),
            Some(0),
        );

        assert_eq!(writer.max_files, None);
        assert_eq!(writer.retention_days, None);

        let built = OwlLoggerBuilder::new().max_files(0).retention_days(0);
        assert_eq!(built.config.max_files, None);
        assert_eq!(built.config.retention_days, None);
    }

    #[test]
    fn zero_retention_days_never_deletes_historical_logs() {
        let temp_dir = std::env::temp_dir().join(format!(
            "owl-test-zero-retention-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let archive = temp_dir.join("app.1.log");
        std::fs::write(&archive, "keep me\n").unwrap();
        let old_time = std::time::UNIX_EPOCH + Duration::from_secs(1_577_836_800);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&archive)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old_time))
            .unwrap();

        cleanup_old_logs(&temp_dir, "app", 0);
        assert!(archive.exists(), "retention_days=0 must not delete logs");
        // 超大天数也不应发生算术乘法溢出
        cleanup_old_logs(&temp_dir, "app", usize::MAX);
        assert!(archive.exists());

        let _ = std::fs::remove_dir_all(&temp_dir);
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

        let config = OwlConfig {
            shutdown_timeout: Some(Duration::ZERO),
            ..OwlConfig::default()
        };
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn cleanup_worker_stops_when_dropped() {
        let worker = CleanupWorker::start(
            std::env::temp_dir(),
            "cleanup-worker-test".to_string(),
            1,
            Arc::new(Mutex::new(())),
        );
        drop(worker);
    }

    #[test]
    fn temp_gzip_path_stays_next_to_destination() {
        let dest = Path::new("/tmp/app.1.log.gz");
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

        std::io::Write::write_all(&mut writer, b"hello day 1\n").unwrap();
        std::io::Write::flush(&mut writer).unwrap();

        let active_path = temp_dir.join("test_daily.log");
        assert!(active_path.exists());

        let yesterday = chrono::Local::now().date_naive() - chrono::Days::new(1);
        writer.active_date = Some(yesterday);

        std::io::Write::write_all(&mut writer, b"hello day 2\n").unwrap();
        std::io::Write::flush(&mut writer).unwrap();
        drop(writer);

        let expected_rotated_name = format!("{}.{}.log", file_name, yesterday.format("%Y-%m-%d"));
        let rotated_path = temp_dir.join(&expected_rotated_name);
        assert!(
            rotated_path.exists(),
            "Expected rotated file to exist: {rotated_path:?}"
        );

        assert!(active_path.exists());
        let active_content = std::fs::read_to_string(&active_path).unwrap();
        assert_eq!(active_content, "hello day 2\n");

        let rotated_content = std::fs::read_to_string(&rotated_path).unwrap();
        assert_eq!(rotated_content, "hello day 1\n");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_owl_rolling_file_writer_utc_rotation() {
        let temp_dir = std::env::temp_dir().join(format!(
            "owl-test-utc-daily-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let mut writer = OwlRollingFileWriter::with_options(
            &temp_dir,
            "utc_app",
            RotationPolicy::Daily,
            None,
            None,
            true,
            Arc::new(Mutex::new(())),
            AtomicCounter::new(),
        );
        std::io::Write::write_all(&mut writer, b"utc day 1\n").unwrap();
        assert_eq!(writer.active_date, Some(chrono::Utc::now().date_naive()));

        let utc_yesterday = chrono::Utc::now().date_naive() - chrono::Days::new(1);
        writer.active_date = Some(utc_yesterday);
        std::io::Write::write_all(&mut writer, b"utc day 2\n").unwrap();
        drop(writer);

        let rotated = temp_dir.join(format!("utc_app.{}.log", utc_yesterday.format("%Y-%m-%d")));
        assert!(rotated.exists(), "expected UTC rotated archive {rotated:?}");
        assert_eq!(std::fs::read_to_string(rotated).unwrap(), "utc day 1\n");

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

        let now = chrono::Local::now();
        let two_hours_ago = now - chrono::Duration::hours(2);
        let active_hour_val = (two_hours_ago.date_naive(), two_hours_ago.hour());
        writer.active_hour = Some(active_hour_val);

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
            "Expected rotated file to exist: {rotated_path:?}"
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

    #[test]
    fn size_rotation_does_not_reuse_deleted_low_index_or_rotate_empty_file() {
        let temp_dir = std::env::temp_dir().join(format!(
            "owl-test-monotonic-size-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

        // 模拟 app.1.log.gz 已被过期清理删除，而 app.5.log.gz 仍存在
        std::fs::write(temp_dir.join("app.5.log.gz"), b"existing").unwrap();

        let mut writer =
            OwlRollingFileWriter::new(&temp_dir, "app", RotationPolicy::SizeMB(1), None, None);
        // 空文件（0 字节）即便遇到超过 max_size 的单条消息也不应生成空归档（通过较小测试块模拟先写入）
        writer.max_size = 10;
        std::io::Write::write_all(&mut writer, b"oversized-initial-line\n").unwrap();
        assert!(
            !temp_dir.join("app.6.log.gz").exists(),
            "0-byte initial file must not be rotated before first write"
        );

        // 下一次写入超过 max_size，应单调递增写入 app.6.log.gz 而非复用已删除的 app.1.log.gz
        std::io::Write::write_all(&mut writer, b"next\n").unwrap();
        drop(writer);

        assert!(
            !temp_dir.join("app.1.log.gz").exists(),
            "deleted low slot app.1.log.gz must not be reused out of order"
        );
        assert!(
            temp_dir.join("app.6.log.gz").exists(),
            "next archive must advance monotonically past existing max index 5"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
