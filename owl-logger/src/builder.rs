use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use crate::config::*;
use crate::error::OwlError;
use crate::formatter;
use crate::guard::OwlGuard;
use crate::i18n::I18n;
use chrono::Timelike;

/// 全局日志过滤器重载句柄，用于在运行期修改日志过滤器级别
pub(crate) static RELOAD_HANDLE: std::sync::OnceLock<
    tracing_subscriber::reload::Handle<EnvFilter, tracing_subscriber::Registry>,
> = std::sync::OnceLock::new();

/// 自定义时间戳格式化器，实现 tracing_subscriber 的 FormatTime
struct OwlTime {
    format: String,
    use_utc: bool,
}

impl tracing_subscriber::fmt::time::FormatTime for OwlTime {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        if self.use_utc {
            write!(w, "{}", chrono::Utc::now().format(&self.format))
        } else {
            write!(w, "{}", chrono::Local::now().format(&self.format))
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
    pub fn max_files(mut self, max_files: usize) -> Self {
        self.config.max_files = Some(max_files);
        self
    }

    /// 设置是否捕获 Panic 并通过日志输出
    pub fn catch_panic(mut self, catch: bool) -> Self {
        self.config.catch_panic = catch;
        self
    }

    /// 添加全局属性字段
    pub fn global_field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.global_fields.insert(key.into(), value.into());
        self
    }

    /// 重新设定敏感词列表
    pub fn sensitive_keys<I, S>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.config.sensitive_keys = keys.into_iter().map(|k| k.into()).collect();
        self
    }

    /// 添加敏感词
    pub fn sensitive_key(mut self, key: impl Into<String>) -> Self {
        self.config.sensitive_keys.push(key.into());
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

    /// 尝试构建并初始化全局日志 subscriber
    pub fn try_init(self) -> Result<OwlGuard, OwlError> {
        let config = self.config;

        // 构建环境过滤器
        let env_filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(config.level.to_string()));

        let mut console_guard: Option<tracing_appender::non_blocking::WorkerGuard> = None;
        let mut file_guard: Option<tracing_appender::non_blocking::WorkerGuard> = None;

        // 公用计时器
        let timer = OwlTime {
            format: config.time_format.clone(),
            use_utc: config.use_utc,
        };

        // 构建控制台输出层
        let console_layer = if config.enable_console {
            let (non_blocking, guard) =
                tracing_appender::non_blocking::NonBlockingBuilder::default()
                    .buffered_lines_limit(config.buffered_lines_limit)
                    .lossy(config.lossy)
                    .finish(std::io::stderr());
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
                        sensitive_keys: config.sensitive_keys.clone(),
                    };
                    tracing_subscriber::fmt::layer()
                        .with_writer(non_blocking)
                        .event_format(json_fmt)
                        .boxed()
                }
                OutputFormat::Compact => tracing_subscriber::fmt::layer()
                    .with_writer(non_blocking)
                    .compact()
                    .with_timer(timer)
                    .with_ansi(config.enable_ansi)
                    .boxed(),
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
        tracing_subscriber::registry()
            .with(env_filter_layer)
            .with(formatter::OwlSpanLayer)
            .with(otel_layer)
            .with(console_layer)
            .with(file_layer)
            .with(error_file_layer)
            .try_init()
            .map_err(|_| OwlError::AlreadyInitialized)?;
        RELOAD_HANDLE
            .set(reload_handle)
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

        // 启动后台过期的日志周期清理线程 (每小时扫描一次)
        if let Some(retention_days) = config.retention_days {
            let log_dir = std::path::PathBuf::from(&config.log_dir);
            let file_name = config.file_name.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
                cleanup_old_logs(&log_dir, &file_name, retention_days);
            });
        }

        // 设置全局语言状态供 #[monitor] 宏查询
        crate::__private::set_language(config.language);

        // 打印初始化成功消息
        tracing::info!("{}", I18n::init_message(config.language));

        Ok(OwlGuard {
            _file_guard: file_guard,
            _console_guard: console_guard,
            _error_file_guard: error_file_guard,
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
                sensitive_keys: config.sensitive_keys.clone(),
            };
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .event_format(json_fmt)
                .boxed()
        }
        OutputFormat::Compact => {
            let timer = OwlTime {
                format: config.time_format.clone(),
                use_utc: config.use_utc,
            };
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .compact()
                .with_timer(timer)
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
            RotationPolicy::SizeMB(mb) => mb * 1024 * 1024,
            _ => 0,
        };
        Self {
            log_dir: log_dir.into(),
            file_name: file_name.into(),
            rotation,
            max_size,
            max_files,
            retention_days,
            current_file: None,
            current_size: 0,
            active_date: None,
            active_hour: None,
        }
    }

    fn init_file(&mut self) -> std::io::Result<&mut std::fs::File> {
        if self.current_file.is_some() {
            return Ok(self.current_file.as_mut().unwrap());
        }

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

        Ok(self.current_file.as_mut().unwrap())
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

                let log_dir = self.log_dir.clone();
                let file_name = self.file_name.clone();
                let rotation = self.rotation.clone();

                std::thread::spawn(move || {
                    let date_str = match rotation {
                        RotationPolicy::Daily => {
                            let d = rotation_date.unwrap_or_else(|| chrono::Local::now().date_naive());
                            d.format("%Y-%m-%d").to_string()
                        }
                        RotationPolicy::Hourly => {
                            let (d, h) = rotation_hour.unwrap_or_else(|| {
                                let now = chrono::Local::now();
                                (now.date_naive(), now.hour())
                            });
                            format!("{}-{:02}", d.format("%Y-%m-%d"), h)
                        }
                        _ => unreachable!(),
                    };

                    let mut dest_path = log_dir.join(format!("{}.{}.log", file_name, date_str));
                    if dest_path.exists() {
                        let mut index = 1;
                        loop {
                            let test_path = log_dir.join(format!("{}.{}.{}.log", file_name, date_str, index));
                            if !test_path.exists() {
                                dest_path = test_path;
                                break;
                            }
                            index += 1;
                        }
                    }
                    let _ = std::fs::rename(&staging_path, dest_path);
                });
            }
            RotationPolicy::Never => {}
        }
        Ok(())
    }

    fn rotate_size(&mut self, file_path: &std::path::Path) -> std::io::Result<()> {
        let staging_path = unique_staging_path(&self.log_dir, &self.file_name);
        std::fs::rename(file_path, &staging_path)?;

        let log_dir = self.log_dir.clone();
        let file_name = self.file_name.clone();
        let max_files = self.max_files;

        std::thread::spawn(move || {
            if let Some(max_files) = max_files {
                if max_files > 1 {
                    let n = max_files - 1;
                    let oldest_gz = log_dir.join(format!("{}.{}.log.gz", file_name, n));
                    let oldest_log = log_dir.join(format!("{}.{}.log", file_name, n));
                    let _ = std::fs::remove_file(oldest_gz);
                    let _ = std::fs::remove_file(oldest_log);

                    for i in (1..n).rev() {
                        let src_gz = log_dir.join(format!("{}.{}.log.gz", file_name, i));
                        let dest_gz = log_dir.join(format!("{}.{}.log.gz", file_name, i + 1));
                        if src_gz.exists() {
                            let _ = std::fs::rename(src_gz, dest_gz);
                        }
                        let src_log = log_dir.join(format!("{}.{}.log", file_name, i));
                        let dest_log = log_dir.join(format!("{}.{}.log", file_name, i + 1));
                        if src_log.exists() {
                            let _ = std::fs::rename(src_log, dest_log);
                        }
                    }

                    let dest_gz = log_dir.join(format!("{}.1.log.gz", file_name));
                    compress_file_sync(staging_path, dest_gz);
                } else {
                    let _ = std::fs::remove_file(staging_path);
                }
            } else {
                let mut index = 1;
                let backup_log = loop {
                    let backup_gz_path = log_dir.join(format!("{}.{}.log.gz", file_name, index));
                    let backup_log_path = log_dir.join(format!("{}.{}.log", file_name, index));
                    if !backup_gz_path.exists() && !backup_log_path.exists() {
                        break backup_log_path;
                    }
                    index += 1;
                };
                let backup_gz = log_dir.join(format!("{}.{}.log.gz", file_name, index));
                if std::fs::rename(&staging_path, &backup_log).is_ok() {
                    compress_file_sync(backup_log, backup_gz);
                } else {
                    let _ = std::fs::remove_file(staging_path);
                }
            }
        });
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

fn compress_file_sync(src_path: std::path::PathBuf, dest_path: std::path::PathBuf) {
    let tmp_path = unique_temp_gzip_path(&dest_path);
    let compress_res = (|| -> std::io::Result<()> {
        let src = std::fs::File::open(&src_path)?;
        let dest = std::fs::File::create(&tmp_path)?;
        let mut encoder = flate2::write::GzEncoder::new(dest, flate2::Compression::default());
        let mut reader = std::io::BufReader::new(src);
        std::io::copy(&mut reader, &mut encoder)?;
        encoder.finish()?;
        if dest_path.exists() {
            let _ = std::fs::remove_file(&dest_path);
        }
        std::fs::rename(&tmp_path, &dest_path)?;
        Ok(())
    })();
    if compress_res.is_ok() {
        let _ = std::fs::remove_file(src_path);
    } else {
        let _ = std::fs::remove_file(tmp_path);
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
    if s.len() != 10 {
        return false;
    }
    let bytes = s.as_bytes();
    bytes[0..4].iter().all(|c| c.is_ascii_digit())
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(|c| c.is_ascii_digit())
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(|c| c.is_ascii_digit())
}

fn is_hourly_date_str(s: &str) -> bool {
    if s.len() != 13 {
        return false;
    }
    let bytes = s.as_bytes();
    bytes[0..4].iter().all(|c| c.is_ascii_digit())
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(|c| c.is_ascii_digit())
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(|c| c.is_ascii_digit())
        && bytes[10] == b'-'
        && bytes[11..13].iter().all(|c| c.is_ascii_digit())
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

    // 2. 匹配旧格式 {file_name}.log.YYYY-MM-DD... (Daily/Hourly 轮转文件)
    let log_dot = format!("{}.log.", file_name);
    if filename.starts_with(&log_dot) {
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
        let temp_dir = std::env::temp_dir().join(format!("owl-test-daily-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let file_name = "test_daily";
        let mut writer = OwlRollingFileWriter::new(
            &temp_dir,
            file_name,
            RotationPolicy::Daily,
            None,
            None,
        );

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

        // Wait a short duration for the background thread to complete the rotation
        std::thread::sleep(std::time::Duration::from_millis(150));

        // The old file should be rotated to test_daily.YYYY-MM-DD.log (using the yesterday's date)
        let expected_rotated_name = format!("{}.{}.log", file_name, yesterday.format("%Y-%m-%d"));
        let rotated_path = temp_dir.join(&expected_rotated_name);
        assert!(rotated_path.exists(), "Expected rotated file to exist: {:?}", rotated_path);

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
        let temp_dir = std::env::temp_dir().join(format!("owl-test-hourly-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let file_name = "test_hourly";
        let mut writer = OwlRollingFileWriter::new(
            &temp_dir,
            file_name,
            RotationPolicy::Hourly,
            None,
            None,
        );

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

        // Wait a short duration for the background thread to complete the rotation
        std::thread::sleep(std::time::Duration::from_millis(150));

        let expected_rotated_name = format!("{}.{}-{:02}.log", file_name, active_hour_val.0.format("%Y-%m-%d"), active_hour_val.1);
        let rotated_path = temp_dir.join(&expected_rotated_name);
        assert!(rotated_path.exists(), "Expected rotated file to exist: {:?}", rotated_path);

        assert!(active_path.exists());
        let active_content = std::fs::read_to_string(&active_path).unwrap();
        assert_eq!(active_content, "hello hour 2\n");

        let rotated_content = std::fs::read_to_string(&rotated_path).unwrap();
        assert_eq!(rotated_content, "hello hour 1\n");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
