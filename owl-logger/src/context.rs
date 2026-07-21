//! 请求上下文追踪模块
//!
//! 借鉴 Python xmi_logger 的 `request_id_var` 功能，基于 `tracing::Span` 实现
//! 自动的请求 ID 注入。在 Span 作用域内的所有日志都会自动带上 `req_id` 字段。
//!
//! # 同步用法
//!
//! ```rust,no_run
//! use owl_logger::context;
//!
//! let _guard = owl_logger::init();
//! let _ctx = context::with_request_id("req-001");
//! tracing::info!("处理请求"); // 自动附带 req_id="req-001"
//! // _ctx Drop 后，后续日志不再附带 req_id
//! ```
//!
//! # 异步用法
//!
//! ```rust,no_run
//! use owl_logger::context;
//! use tracing::Instrument;
//!
//! async fn handle_request() {
//!     let span = context::request_span("req-002");
//!     async {
//!         tracing::info!("异步处理中");
//!     }
//!     .instrument(span)
//!     .await;
//! }
//! ```

/// 创建一个带 `req_id` 字段的 Span 并进入上下文（同步）
///
/// 返回的 `EnteredSpan` 对象在被丢弃时自动退出 Span。
/// 在此 Span 有效期间，所有日志事件都会自动附带 `req_id` 字段。
pub fn with_request_id(request_id: &str) -> tracing::span::EnteredSpan {
    // 使用 ERROR 级别，保证全局过滤器仅保留错误日志时上下文仍然存在。
    let span = tracing::error_span!("request", req_id = %request_id);
    span.entered()
}

/// 创建一个带 `req_id` 字段的 Span（异步）
///
/// 返回的 `Span` 可以配合 `.instrument()` 在异步任务中使用。
///
/// ```rust,no_run
/// use tracing::Instrument;
///
/// # async fn example() {
/// let span = owl_logger::context::request_span("req-003");
/// # async fn some_async_fn() {}
/// some_async_fn().instrument(span).await;
/// # }
/// ```
pub fn request_span(request_id: &str) -> tracing::Span {
    tracing::error_span!("request", req_id = %request_id)
}

/// 创建一个带自定义标签和值的 Span（同步）
///
/// tracing 的 span 名和字段名必须在编译期确定，因此此函数会创建名为 context 的
/// span，并记录 context_name、field_name、field_value 三个字段。若需要真正的静态
/// span 名/字段名，请使用 crate 根导出的 context_span 宏。
pub fn with_context(
    name: &'static str,
    id_field: &str,
    id_value: &str,
) -> tracing::span::EnteredSpan {
    let span = tracing::error_span!(
        "context",
        context_name = %name,
        field_name = %id_field,
        field_value = %id_value
    );
    span.entered()
}
