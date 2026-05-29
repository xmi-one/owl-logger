//! 基础用法示例
//!
//! 运行：`cargo run --example basic`

use owl_logger::{Language, LogLevel, RotationPolicy};

fn main() {
    // 带配置的初始化
    let _guard = owl_logger::builder()
        .file_name("basic_example")
        .log_dir("logs")
        .language(Language::Zh)
        .level(LogLevel::Trace)
        .rotation(RotationPolicy::Daily)
        .show_target(true)
        .show_line_number(true)
        .init();

    // 使用各种日志级别
    owl_logger::trace!("这是追踪级别日志");
    owl_logger::debug!("这是调试级别日志");
    owl_logger::info!("这是信息级别日志");
    owl_logger::warn!("这是警告级别日志");
    owl_logger::error!("这是错误级别日志");

    // 结构化日志
    owl_logger::info!(
        user = "alice",
        action = "login",
        ip = "192.168.1.100",
        "用户登录成功"
    );

    // 请求上下文追踪
    {
        let _ctx = owl_logger::context::with_request_id("req-001");
        owl_logger::info!("开始处理订单");
        owl_logger::info!(order_id = "ORD-12345", amount = 99.9, "订单创建成功");
        owl_logger::warn!(stock = 3, "库存不足，请及时补货");
    }
    // _ctx 被 Drop，后续日志不再带 req_id

    owl_logger::info!("这条日志没有 req_id");

    // 使用 #[monitor] 宏
    let result = process_order("ORD-12345", 199.9);
    owl_logger::info!(result = result, "订单处理完成");

    // Guard 在 main 结束时被 Drop，自动 flush 所有日志
}

#[owl_logger::monitor]
fn process_order(order_id: &str, amount: f64) -> bool {
    owl_logger::info!("正在验证订单...");
    std::thread::sleep(std::time::Duration::from_millis(50));
    owl_logger::info!("订单验证通过");
    true
}
