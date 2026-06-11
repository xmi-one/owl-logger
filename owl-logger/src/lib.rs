//! # 🦉 owl-logger
//!
//! **开箱即用、生产级、Rust 风格的日志库**
//!
//! 基于 `tracing` 生态构建，借鉴 Python `xmi_logger` 的设计理念，
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
//! use owl_logger::{Language, LogLevel, RotationPolicy};
//!
//! let _guard = owl_logger::builder()
//!     .file_name("my_app")
//!     .log_dir("logs")
//!     .language(Language::Zh)
//!     .level(LogLevel::Debug)
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

mod builder;
mod config;
pub mod context;
mod error;
mod formatter;
mod guard;
mod i18n;

// ===== 公开 API =====

pub use builder::OwlLoggerBuilder;
pub use config::{Language, LogLevel, OutputFormat, RotationPolicy};
pub use error::OwlError;
pub use guard::OwlGuard;

// Re-export 过程宏
pub use owl_logger_macros::monitor;

// Re-export tracing 核心宏，方便用户不必额外 `use tracing`
pub use tracing::instrument;
pub use tracing::Instrument;
pub use tracing::{debug, error, info, trace, warn};
pub use tracing::{debug_span, error_span, info_span, trace_span, warn_span};

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
/// - `OWL_LOG_FORMAT`: `pretty` / `compact` / `json`
/// - `OWL_LOG_DIR`: 日志目录
/// - `OWL_LOG_FILE`: 日志文件名前缀
pub fn builder_from_env() -> Result<OwlLoggerBuilder, OwlError> {
    OwlLoggerBuilder::from_env()
}

/// 零配置一键初始化
///
/// 使用默认配置初始化日志系统：
/// - 控制台 + 文件双输出
/// - Info 级别
/// - 英文
/// - Pretty 格式
/// - Daily 文件轮转
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

/// 动态获取当前的日志过滤器规则
pub fn get_filter() -> Result<String, OwlError> {
    if let Some(handle) = crate::builder::RELOAD_HANDLE.get() {
        Ok(handle
            .with_current(|filter| filter.to_string())
            .unwrap_or_default())
    } else {
        Err(OwlError::NotInitialized)
    }
}

/// 动态更新全局日志过滤器规则 (例如 "info,my_crate=debug")
pub fn set_filter(filter_str: impl AsRef<str>) -> Result<(), OwlError> {
    if let Some(handle) = crate::builder::RELOAD_HANDLE.get() {
        let filter_str = filter_str.as_ref();
        let new_filter = tracing_subscriber::EnvFilter::try_new(filter_str)
            .map_err(|e| OwlError::EnvFilter(e.to_string()))?;
        handle
            .reload(new_filter)
            .map_err(|e| OwlError::Reload(e.to_string()))?;
        Ok(())
    } else {
        Err(OwlError::NotInitialized)
    }
}

/// 动态更新全局日志级别
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

    // 用于 #[monitor] 宏的 Autoref 特化检测工具，自动识别 Result::Err 返回值
    pub struct OwlWrap<T>(pub T);

    #[derive(Debug, Clone)]
    pub struct OwlResultInfo {
        pub is_err: bool,
        pub level_override: Option<tracing::Level>,
        pub error_msg: Option<String>,
    }

    pub trait OwlLowPriority {
        fn owl_inspect(&self) -> OwlResultInfo;
    }

    impl<T> OwlLowPriority for &OwlWrap<T> {
        #[inline]
        fn owl_inspect(&self) -> OwlResultInfo {
            OwlResultInfo {
                is_err: false,
                level_override: None,
                error_msg: None,
            }
        }
    }

    pub trait OwlHighPriority {
        fn owl_inspect(&self) -> OwlResultInfo;
    }

    impl<T, E: std::fmt::Debug> OwlHighPriority for OwlWrap<&Result<T, E>> {
        #[inline]
        fn owl_inspect(&self) -> OwlResultInfo {
            match self.0 {
                Ok(_) => OwlResultInfo {
                    is_err: false,
                    level_override: None,
                    error_msg: None,
                },
                Err(e) => OwlResultInfo {
                    is_err: true,
                    level_override: Some(tracing::Level::ERROR),
                    error_msg: Some(format!("{:?}", e)),
                },
            }
        }
    }
}
