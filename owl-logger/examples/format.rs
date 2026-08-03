//! 输出格式展示示例
//!
//! 运行：
//! - Pretty 格式（默认）：`cargo run --example format -p owl-logger`
//! - JSON 格式：`FORMAT=json cargo run --example format -p owl-logger`
//! - Compact 格式：`FORMAT=compact cargo run --example format -p owl-logger`

use owl_logger::{LogLevel, OutputFormat};

fn main() {
    // 根据环境变量 FORMAT 选择输出格式
    let format_str = std::env::var("FORMAT").unwrap_or_default().to_lowercase();
    let format = match format_str.as_str() {
        "json" => OutputFormat::Json,
        "compact" => OutputFormat::Compact,
        _ => OutputFormat::Pretty,
    };

    println!(">>> 正在以 {format:?} 格式初始化 owl-logger ...\n");

    let _guard = owl_logger::builder()
        .format(format)
        .level(LogLevel::Trace)
        .init();

    owl_logger::trace!("这是追踪级别日志");
    owl_logger::debug!("这是调试级别日志");
    owl_logger::info!("这是信息级别日志");
    owl_logger::warn!(code = 404, "这是警告级别日志");
    owl_logger::error!(reason = "connection timeout", "这是错误级别日志");

    // 上下文追踪在各种格式下的呈现
    {
        let _ctx = owl_logger::context::with_request_id("req-999");
        owl_logger::info!("这是带请求 ID 的上下文日志");
    }
}
