//! # 🦉 owl-logger
//!
//! **开箱即用、生产级、Rust 风格的日志库**
//!
//! 基于 `tracing` 生态构建，借鉴 Python `yyds-logger` / `xmi_logger` 的设计理念，
//! 提供简洁的 API 和丰富的功能。
//!
//! ## 快速开始
//!
//! ```rust,no_run
//! let _guard = owl_logger::init();
//!
//! tracing::info!("Hello from owl-logger! 🦉");
//! tracing::warn!(user = "alice", "Something needs attention");
//! tracing::error!("Oops, something went wrong");
//! ```
//!
//! ## 自定义配置
//!
//! ```rust,no_run
//! use owl_logger::{Language, LogLevel, OutputFormat, RotationPolicy};
//!
//! let _guard = owl_logger::builder()
//!     .file_name("my_app")
//!     .log_dir("logs")
//!     .language(Language::Zh)
//!     .level(LogLevel::Debug)
//!     .console_format(OutputFormat::Pretty)
//!     .file_format(OutputFormat::Json)
//!     .rotation(RotationPolicy::Daily)
//!     .init();
//!
//! tracing::info!("🦉 开始工作！");
//! ```
//!
//! ## 请求上下文追踪
//!
//! ```rust,no_run
//! let _guard = owl_logger::init();
//!
//! // 同步上下文
//! let _ctx = owl_logger::context::with_request_id("req-001");
//! tracing::info!("处理订单"); // 日志自动带上 req_id="req-001"
//! ```
//!
//! ## 重要提示
//!
//! `init()` 和 `builder().init()` 返回的 `OwlGuard` **必须被持有**（通常用 `let _guard = ...`）。
//! 当 Guard 被丢弃时，会自动 flush 所有缓冲的日志。如果不持有 Guard，日志可能会丢失。

// 供 #[monitor] 在库本体、示例和外部依赖重命名场景中使用统一的绝对路径。
extern crate self as owl_logger;

mod builder;
mod config;
pub mod context;
mod error;
mod formatter;
mod guard;
mod i18n;
mod writer;

// ===== 公开 API =====

pub use builder::OwlLoggerBuilder;
pub use config::{Language, LogLevel, OutputFormat, RotationPolicy};
pub use error::OwlError;
pub use guard::{DroppedLogLines, OwlGuard, SinkErrors};

/// 函数自动监控属性宏（参见 `owl_logger_macros::monitor`）。
///
/// ```rust
/// use owl_logger::monitor;
///
/// #[monitor(skip(password))]
/// fn login_ok(user: &str, password: &str) -> bool {
///     !user.is_empty() && !password.is_empty()
/// }
/// ```
///
/// 若 `skip(...)` 中拼写错误传入不存在的参数名，编译期将直接报错阻止敏感信息意外输出：
///
/// ```compile_fail
/// use owl_logger::monitor;
///
/// #[monitor(skip(pasword))]
/// fn login_bad(user: &str, password: &str) -> bool {
///     !user.is_empty() && !password.is_empty()
/// }
/// ```
pub use owl_logger_macros::monitor;

// Re-export tracing 核心宏，方便用户不必额外 `use tracing`
pub use tracing::instrument;
pub use tracing::Instrument;
pub use tracing::{debug, error, info, trace, warn};
pub use tracing::{debug_span, error_span, info_span, trace_span, warn_span};

/// 创建一个具有编译期确定名称和字段的上下文 Span。
///
/// 该 macro 使用 ERROR 级别，使请求/业务上下文也能在只保留错误日志的生产过滤器中
/// 被记录，同时完整保留原生字段类型（如整数、布尔值在 JSON 中保持原始类型，并支持 `%` / `?` 修饰符）。
///
/// ```rust,no_run
/// let span = owl_logger::context_span!("order", order_id = "ORD-1", attempt = 1);
/// let _entered = span.enter();
/// ```
#[macro_export]
macro_rules! context_span {
    ($name:literal $(, $($fields:tt)*)?) => {
        $crate::error_span!($name $(, $($fields)*)?)
    };
}

/// 创建一个新的 Builder 实例
///
/// # 示例
///
/// ```rust,no_run
/// let _guard = owl_logger::builder()
///     .file_name("app")
///     .language(owl_logger::Language::Zh)
///     .init();
/// ```
pub fn builder() -> OwlLoggerBuilder {
    OwlLoggerBuilder::new()
}

/// 从环境变量创建 Builder 实例
///
/// 当前支持：
/// - `OWL_LOG_LEVEL`: `trace` / `debug` / `info` / `warn` / `error`
/// - `OWL_LOG_CONSOLE_LEVEL` / `OWL_LOG_FILE_LEVEL`: 独立通道级别（或 `off`）
/// - `OWL_LOG_ERROR_FILE`: 分级独立文件最低级别（如 `error` / `warn`，或 `off`）
/// - `OWL_LOG_FORMAT`: `pretty` / `compact` / `json`
/// - `OWL_LOG_CONSOLE_FORMAT` / `OWL_LOG_FILE_FORMAT`: 独立通道格式
/// - `OWL_LOG_ROTATION`: `daily` / `hourly` / `never` / `<N>mb`（如 `50mb`）
/// - `OWL_LOG_LANG`: `en` / `zh`
/// - `OWL_LOG_CONSOLE` / `OWL_LOG_FILE_ENABLE` / `OWL_LOG_UTC` / `OWL_LOG_PROCESS_ISOLATION`: 布尔开关
/// - `OWL_LOG_RETENTION_DAYS` / `OWL_LOG_MAX_FILES`: 非负整数（`0` 表示不限制）
/// - `OWL_LOG_DIR`: 日志目录
/// - `OWL_LOG_FILE`: 日志文件名前缀
/// - `OTEL_EXPORTER_OTLP_ENDPOINT` / `OTEL_SERVICE_NAME`: OTLP 导出配置
///
/// 若 `RUST_LOG` 存在且是有效的 tracing filter，它会优先于基础 level 配置生效。
pub fn builder_from_env() -> Result<OwlLoggerBuilder, OwlError> {
    OwlLoggerBuilder::from_env()
}

/// 零配置一键初始化
///
/// # 示例
///
/// ```rust,no_run
/// let _guard = owl_logger::init();
/// tracing::info!("ready!");
/// ```
///
/// # Panics
///
/// 如果全局 subscriber 已经被设置，将会 panic。
pub fn init() -> OwlGuard {
    builder().init()
}

/// 尝试零配置初始化（不 panic）
///
/// 与 `init()` 相同，但失败时返回 `Err` 而非 panic。
pub fn try_init() -> Result<OwlGuard, OwlError> {
    builder().try_init()
}

/// 从环境变量初始化日志系统
pub fn try_init_from_env() -> Result<OwlGuard, OwlError> {
    builder_from_env()?.try_init()
}

/// 主动排空当前全局日志系统的所有异步缓冲队列并刷盘（不关闭日志系统）。
pub fn flush() -> Result<(), OwlError> {
    if let Some(handle) = crate::builder::FLUSH_HANDLE.get() {
        handle.flush()
    } else {
        Err(OwlError::NotInitialized)
    }
}

/// 动态获取当前的日志过滤器规则
pub fn get_filter() -> Result<String, OwlError> {
    if let Some(handle) = crate::builder::RELOAD_HANDLE.get() {
        handle.current_filter().map_err(OwlError::Reload)
    } else {
        Err(OwlError::NotInitialized)
    }
}

/// 动态更新全局日志过滤器规则 (例如 `"info,my_crate=debug"` 或模块作用域规则 `"my_crate=info"`)
pub fn set_filter(filter_str: impl AsRef<str>) -> Result<(), OwlError> {
    if let Some(handle) = crate::builder::RELOAD_HANDLE.get() {
        let catch_panic =
            crate::builder::CATCH_PANIC_ENABLED.load(std::sync::atomic::Ordering::Relaxed);
        let new_filter =
            crate::builder::build_normalized_env_filter(filter_str.as_ref(), catch_panic)?;
        handle.reload_filter(new_filter).map_err(OwlError::Reload)?;
        Ok(())
    } else {
        Err(OwlError::NotInitialized)
    }
}

/// 动态更新全局日志级别。
///
/// 此操作会用一个新的全局级别替换当前完整 filter；若需要保留目标级别规则，请使用
/// set_filter。
pub fn set_level(level: LogLevel) -> Result<(), OwlError> {
    set_filter(level.to_string())
}

/// 仅供过程宏内部使用的私有 API
#[doc(hidden)]
pub mod __private {
    pub use crate::config::Language;
    pub use crate::i18n::I18n;

    // Re-export 整个 tracing crate，使 `#[monitor]` 宏展开后无需用户显式依赖 tracing
    pub use tracing;

    use std::sync::atomic::{AtomicU8, Ordering};

    pub const DEFAULT_MONITOR_MAX_LEN: usize = 1024;

    /// 显式以 `FnOnce` 边界调用同步监控闭包，阻止编译器将 `move ||` 推断为 `FnMut`，
    /// 从而允许 `&mut self -> &mut T` 及 `&mut [T] -> Result<&mut T, E>` 等可变借用安全返回。
    #[inline(always)]
    pub fn call_once<R, F: FnOnce() -> R>(f: F) -> R {
        f()
    }

    static CURRENT_LANG: AtomicU8 = AtomicU8::new(0); // 0 = En, 1 = Zh

    pub fn set_language(lang: Language) {
        let val = match lang {
            Language::En => 0,
            Language::Zh => 1,
        };
        CURRENT_LANG.store(val, Ordering::Relaxed);
    }

    pub fn get_language() -> Language {
        match CURRENT_LANG.load(Ordering::Relaxed) {
            1 => Language::Zh,
            _ => Language::En,
        }
    }

    /// 带字节上限短路保护的 `Debug` 格式化，遇到超大集合/字节切片在到达上限时立即停止遍历，
    /// 避免在热路径中分配数十 MB 的无用字符串。
    pub fn write_debug_truncated<T: std::fmt::Debug + ?Sized>(
        out: &mut String,
        value: &T,
        max_len: usize,
    ) {
        use std::fmt::Write as _;
        if max_len == 0 {
            let _ = write!(out, "{value:?}");
            return;
        }

        struct BoundedWriter<'a> {
            out: &'a mut String,
            remaining: usize,
            truncated: bool,
        }

        impl std::fmt::Write for BoundedWriter<'_> {
            fn write_str(&mut self, s: &str) -> std::fmt::Result {
                if self.remaining == 0 {
                    self.truncated = true;
                    return Err(std::fmt::Error);
                }
                if s.len() <= self.remaining {
                    self.out.push_str(s);
                    self.remaining -= s.len();
                    Ok(())
                } else {
                    let mut end = self.remaining;
                    while end > 0 && !s.is_char_boundary(end) {
                        end -= 1;
                    }
                    self.out.push_str(&s[..end]);
                    self.remaining = 0;
                    self.truncated = true;
                    Err(std::fmt::Error)
                }
            }
        }

        let mut writer = BoundedWriter {
            out,
            remaining: max_len,
            truncated: false,
        };
        let _ = write!(&mut writer, "{value:?}");
        if writer.truncated {
            out.push_str("...");
        }
    }

    pub fn format_debug_truncated<T: std::fmt::Debug + ?Sized>(
        value: &T,
        max_len: usize,
    ) -> String {
        let mut out = String::new();
        write_debug_truncated(&mut out, value, max_len);
        out
    }

    // 用于 #[monitor] 宏的 Autoref 特化检测工具，自动识别 Result::Err 返回值
    pub struct OwlWrap<T>(pub T);

    #[derive(Debug, Clone)]
    pub struct OwlResultInfo {
        pub is_err: bool,
        pub level_override: Option<tracing::Level>,
        pub error_msg: Option<String>,
    }

    pub trait OwlLowPriority {
        fn owl_inspect(&self) -> OwlResultInfo {
            self.owl_inspect_bounded(DEFAULT_MONITOR_MAX_LEN)
        }
        fn owl_inspect_bounded(&self, _max_len: usize) -> OwlResultInfo;
    }

    impl<T> OwlLowPriority for &OwlWrap<T> {
        #[inline]
        fn owl_inspect_bounded(&self, _max_len: usize) -> OwlResultInfo {
            OwlResultInfo {
                is_err: false,
                level_override: None,
                error_msg: None,
            }
        }
    }

    pub trait OwlHighPriority {
        fn owl_inspect(&self) -> OwlResultInfo {
            self.owl_inspect_bounded(DEFAULT_MONITOR_MAX_LEN)
        }
        fn owl_inspect_bounded(&self, max_len: usize) -> OwlResultInfo;
    }

    impl<T, E: std::fmt::Debug> OwlHighPriority for OwlWrap<&Result<T, E>> {
        #[inline]
        fn owl_inspect_bounded(&self, max_len: usize) -> OwlResultInfo {
            match self.0 {
                Ok(_) => OwlResultInfo {
                    is_err: false,
                    level_override: None,
                    error_msg: None,
                },
                Err(e) => OwlResultInfo {
                    is_err: true,
                    level_override: Some(tracing::Level::ERROR),
                    error_msg: Some(format_debug_truncated(e, max_len)),
                },
            }
        }
    }
}
