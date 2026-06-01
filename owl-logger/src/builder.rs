use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use crate::config::*;
use crate::error::OwlError;
use crate::formatter;
use crate::guard::OwlGuard;
use crate::i18n::I18n;

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
        let ts = if self.use_utc {
            chrono::Utc::now().format(&self.format).to_string()
        } else {
            chrono::Local::now().format(&self.format).to_string()
        };
        write!(w, "{}", ts)
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

        // 构建文件输出层
        let file_layer = if config.enable_file {
            std::fs::create_dir_all(&config.log_dir).map_err(OwlError::LogDirCreation)?;

            // 启动时先清理一次过期日志
            if let Some(retention_days) = config.retention_days {
                cleanup_old_logs(
                    std::path::Path::new(&config.log_dir),
                    &config.file_name,
                    retention_days,
                );
            }

            let file_writer: Box<dyn std::io::Write + Send + Sync + 'static> =
                match &config.rotation {
                    RotationPolicy::Daily => {
                        let mut builder = tracing_appender::rolling::RollingFileAppender::builder()
                            .rotation(tracing_appender::rolling::Rotation::DAILY)
                            .filename_prefix(&config.file_name)
                            .filename_suffix("log");
                        if let Some(max_files) = config.max_files {
                            builder = builder.max_log_files(max_files);
                        }
                        Box::new(
                            builder
                                .build(&config.log_dir)
                                .map_err(|e| OwlError::FileAppenderCreation(e.to_string()))?,
                        )
                    }
                    RotationPolicy::Hourly => {
                        let mut builder = tracing_appender::rolling::RollingFileAppender::builder()
                            .rotation(tracing_appender::rolling::Rotation::HOURLY)
                            .filename_prefix(&config.file_name)
                            .filename_suffix("log");
                        if let Some(max_files) = config.max_files {
                            builder = builder.max_log_files(max_files);
                        }
                        Box::new(
                            builder
                                .build(&config.log_dir)
                                .map_err(|e| OwlError::FileAppenderCreation(e.to_string()))?,
                        )
                    }
                    RotationPolicy::SizeMB(mb) => Box::new(SizeRotatingFileWriter::new(
                        &config.log_dir,
                        &config.file_name,
                        *mb,
                        config.max_files,
                        config.retention_days,
                    )),
                    RotationPolicy::Never => {
                        let mut builder = tracing_appender::rolling::RollingFileAppender::builder()
                            .rotation(tracing_appender::rolling::Rotation::NEVER)
                            .filename_prefix(&config.file_name)
                            .filename_suffix("log");
                        if let Some(max_files) = config.max_files {
                            builder = builder.max_log_files(max_files);
                        }
                        Box::new(
                            builder
                                .build(&config.log_dir)
                                .map_err(|e| OwlError::FileAppenderCreation(e.to_string()))?,
                        )
                    }
                };

            let (non_blocking, guard) =
                tracing_appender::non_blocking::NonBlockingBuilder::default()
                    .buffered_lines_limit(config.buffered_lines_limit)
                    .lossy(config.lossy)
                    .finish(file_writer);
            file_guard = Some(guard);

            let timer = OwlTime {
                format: config.time_format.clone(),
                use_utc: config.use_utc,
            };

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
                    .with_ansi(false)
                    .boxed(),
                OutputFormat::Pretty => {
                    let fmt = formatter::file_formatter(config.language, &config);
                    tracing_subscriber::fmt::layer()
                        .with_writer(non_blocking)
                        .event_format(fmt)
                        .with_ansi(false)
                        .boxed()
                }
            };
            Some(layer)
        } else {
            None
        };

        // 包装 reloadable filter 层
        let (env_filter_layer, reload_handle) = tracing_subscriber::reload::Layer::new(env_filter);

        // 初始化全局注册表
        tracing_subscriber::registry()
            .with(env_filter_layer)
            .with(console_layer)
            .with(file_layer)
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
            language: config.language,
        })
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

/// 支持按文件大小限制自动轮转，且在后台线程进行 Gzip 压缩与保留清理的自定义文件写入器
struct SizeRotatingFileWriter {
    log_dir: std::path::PathBuf,
    file_name: String,
    max_size: u64,
    max_files: Option<usize>,
    retention_days: Option<usize>,
    current_file: Option<std::fs::File>,
    current_size: u64,
}

impl SizeRotatingFileWriter {
    pub fn new(
        log_dir: impl Into<std::path::PathBuf>,
        file_name: impl Into<String>,
        max_size_mb: u64,
        max_files: Option<usize>,
        retention_days: Option<usize>,
    ) -> Self {
        Self {
            log_dir: log_dir.into(),
            file_name: file_name.into(),
            max_size: max_size_mb * 1024 * 1024,
            max_files,
            retention_days,
            current_file: None,
            current_size: 0,
        }
    }

    fn init_file(&mut self) -> std::io::Result<&mut std::fs::File> {
        if self.current_file.is_some() {
            return Ok(self.current_file.as_mut().unwrap());
        }

        let file_path = self.log_dir.join(format!("{}.log", self.file_name));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)?;

        let metadata = file.metadata()?;
        self.current_size = metadata.len();
        self.current_file = Some(file);

        Ok(self.current_file.as_mut().unwrap())
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        self.current_file = None;

        let file_path = self.log_dir.join(format!("{}.log", self.file_name));
        if file_path.exists() {
            if let Some(max_files) = self.max_files {
                if max_files > 1 {
                    let n = max_files - 1;
                    // 1. 删除最老的一个压缩备份文件 app.N-1.log.gz 和可能存留的原始 log
                    let oldest_gz = self
                        .log_dir
                        .join(format!("{}.{}.log.gz", self.file_name, n));
                    let oldest_log = self.log_dir.join(format!("{}.{}.log", self.file_name, n));
                    let _ = std::fs::remove_file(oldest_gz);
                    let _ = std::fs::remove_file(oldest_log);

                    // 2. 依次将 app.i.log.gz (及 .log) 顺延重命名为 app.i+1.log.gz (或 .log)
                    for i in (1..n).rev() {
                        let src_gz = self
                            .log_dir
                            .join(format!("{}.{}.log.gz", self.file_name, i));
                        let dest_gz =
                            self.log_dir
                                .join(format!("{}.{}.log.gz", self.file_name, i + 1));
                        if src_gz.exists() {
                            std::fs::rename(src_gz, dest_gz)?;
                        }
                        let src_log = self.log_dir.join(format!("{}.{}.log", self.file_name, i));
                        let dest_log =
                            self.log_dir
                                .join(format!("{}.{}.log", self.file_name, i + 1));
                        if src_log.exists() {
                            std::fs::rename(src_log, dest_log)?;
                        }
                    }

                    // 3. 将当前的 app.log 重命名为 app.1.log
                    let dest = self.log_dir.join(format!("{}.1.log", self.file_name));
                    std::fs::rename(&file_path, &dest)?;

                    // 4. 后台压缩 app.1.log -> app.1.log.gz
                    let dest_gz = self.log_dir.join(format!("{}.1.log.gz", self.file_name));
                    compress_file_in_background(dest, dest_gz);
                } else {
                    // max_files == 1: 直接删除当前日志文件，不保存任何备份
                    let _ = std::fs::remove_file(&file_path);
                }
            } else {
                // 不限制最大文件数量，寻找下一个空闲的压缩文件索引 app.index.log.gz
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
                std::fs::rename(&file_path, &backup_log)?;
                compress_file_in_background(backup_log, backup_gz);
            }
        }

        // 重新清理过期日志
        if let Some(retention_days) = self.retention_days {
            cleanup_old_logs(&self.log_dir, &self.file_name, retention_days);
        }

        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&file_path)?;

        self.current_size = 0;
        self.current_file = Some(file);
        Ok(())
    }
}

impl std::io::Write for SizeRotatingFileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.init_file()?;
        if self.current_size + buf.len() as u64 > self.max_size {
            self.rotate()?;
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

/// 后台压缩文件逻辑
fn compress_file_in_background(src_path: std::path::PathBuf, dest_path: std::path::PathBuf) {
    std::thread::spawn(move || {
        let compress_res = (|| -> std::io::Result<()> {
            let src = std::fs::File::open(&src_path)?;
            let tmp_path = unique_temp_gzip_path(&dest_path);
            let dest = std::fs::File::create(&tmp_path)?;
            let mut encoder = flate2::write::GzEncoder::new(dest, flate2::Compression::default());
            let mut reader = std::io::BufReader::new(src);
            std::io::copy(&mut reader, &mut encoder)?;
            encoder.finish()?;
            if dest_path.exists() {
                let _ = std::fs::remove_file(&dest_path);
            }
            std::fs::rename(tmp_path, &dest_path)?;
            Ok(())
        })();
        if compress_res.is_ok() {
            let _ = std::fs::remove_file(src_path);
        }
    });
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

/// 清理过期日志文件
fn cleanup_old_logs(log_dir: &std::path::Path, file_name: &str, retention_days: usize) {
    let now = std::time::SystemTime::now();
    let max_age = std::time::Duration::from_secs(retention_days as u64 * 24 * 60 * 60);

    if let Ok(entries) = std::fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(filename_str) = path.file_name().and_then(|s| s.to_str()) {
                    let is_exact_or_dotted = filename_str == format!("{}.log", file_name)
                        || filename_str == format!("{}.log.gz", file_name)
                        || filename_str.starts_with(&format!("{}.", file_name));

                    if is_exact_or_dotted {
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
}
