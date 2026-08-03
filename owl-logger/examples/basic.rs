//! 基础与全特性用法示例
//!
//! 运行：`cargo run --example basic -p owl-logger`

use owl_logger::{Language, LogLevel, RotationPolicy};

fn main() {
    // 1. 全配置初始化
    println!(">>> 正在以全配置初始化 owl-logger ...\n");
    let _guard = owl_logger::builder()
        .file_name("basic_example")
        .log_dir("logs")
        .language(Language::Zh)
        .level(LogLevel::Trace)
        .rotation(RotationPolicy::Daily)
        .console(true)
        .file(true)
        .ansi(true)
        .show_target(true)
        .show_line_number(true)
        .show_thread(true) // 测试线程名称输出
        .init();

    // 2. 测试重复初始化（try_init 错误处理）
    println!("\n>>> 测试重复初始化 logger（应当安全返回错误）：");
    match owl_logger::builder().try_init() {
        Ok(_) => owl_logger::warn!("警告：重复初始化未报错（不合预期）"),
        Err(e) => owl_logger::info!(error = %e, "成功捕获到预期的重复初始化错误"),
    }

    // 3. 测试 5 种级别的日志输出与彩色高亮
    println!("\n>>> 测试各种级别日志消息着色与格式：");
    owl_logger::trace!("这是 TRACE (追踪) 级别日志（应当呈暗淡灰色）");
    owl_logger::debug!("这是 DEBUG (调试) 级别日志（应当呈暗淡灰色）");
    owl_logger::info!("这是 INFO (信息) 级别日志（应当呈默认颜色）");
    owl_logger::warn!("这是 WARN (警告) 级别日志（应当呈黄色）");
    owl_logger::error!("这是 ERROR (错误) 级别日志（应当呈红色加粗）");

    // 4. 测试结构化日志字段
    println!("\n>>> 测试结构化字段输出：");
    owl_logger::info!(
        user = "alice",
        action = "login",
        ip = "192.168.1.100",
        "用户登录成功"
    );

    // 5. 测试标准 log crate 桥接
    println!("\n>>> 测试标准 log crate 桥接：");
    log::info!("这是一条来自标准 log crate 的 INFO 日志");
    log::warn!("这是一条来自标准 log crate 的 WARN 日志");

    // 6. 测试请求上下文追踪
    println!("\n>>> 测试请求上下文追踪 (with_request_id)：");
    {
        let _ctx = owl_logger::context::with_request_id("req-10086");
        owl_logger::info!("开始处理核心订单业务");
        owl_logger::info!(order_id = "ORD-9999", amount = 888.8, "订单创建成功");
        owl_logger::warn!(stock = 0, "库存售罄！");
    }
    // Context 被 Drop 释放
    owl_logger::info!("外部日志，不再带有 req_id");

    // 7. 测试 #[monitor] 函数监控宏（默认 INFO 级别）
    println!("\n>>> 测试 #[monitor] 属性宏（INFO 级别，支持返回值/耗时/入参）：");
    let result = process_order("ORD-12345", 199.9);
    owl_logger::info!(result = result, "订单处理完成");

    // 8. 测试 #[monitor] 属性宏（自定义 DEBUG 级别）
    println!("\n>>> 测试 #[monitor] 属性宏（DEBUG 级别）：");
    let calculated = calculate_discount(100.0, 0.8);
    owl_logger::debug!(calculated = calculated, "折扣计算完成");

    // 9. 测试 #[monitor] 属性宏（WARN 级别，省略 password 参数）
    println!("\n>>> 测试 #[monitor] 属性宏（WARN 级别 + skip 参数）：");
    let logged_in = login("admin", "super_secret_password");
    owl_logger::info!(logged_in = logged_in, "登录验证结束");

    println!("\n>>> 示例运行结束，开始 Drop Guard 清理日志：");
}

// 7. 默认级别监控
#[owl_logger::monitor]
fn process_order(order_id: &str, amount: f64) -> bool {
    owl_logger::info!("正在验证订单信息...");
    std::thread::sleep(std::time::Duration::from_millis(30));
    true
}

// 8. 自定义级别为 debug
#[owl_logger::monitor(level = "debug")]
fn calculate_discount(price: f64, rate: f64) -> f64 {
    price * rate
}

// 9. 自定义级别为 warn，省略 password 参数
#[owl_logger::monitor(level = "warn", skip(password))]
fn login(username: &str, password: &str) -> bool {
    owl_logger::warn!("正在尝试非安全登录连接...");
    username == "admin" && !password.is_empty()
}
