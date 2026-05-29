//! 演示全局字段、敏感词脱敏、高并发队列限制、以及宏结果级别升级等高级功能
//!
//! 运行：
//! - Pretty 格式：`cargo run --example advanced_features -p owl-logger`
//! - JSON 格式：`FORMAT=json cargo run --example advanced_features -p owl-logger`

use owl_logger::monitor;

#[monitor]
fn perform_success_action(user: &str) -> Result<String, String> {
    Ok(format!("Welcome, {}!", user))
}

#[monitor]
fn perform_error_action(user: &str) -> Result<String, String> {
    Err(format!("Access denied for user: {}", user))
}

fn main() {
    let format_str = std::env::var("FORMAT").unwrap_or_default().to_lowercase();
    let format = match format_str.as_str() {
        "json" => owl_logger::OutputFormat::Json,
        _ => owl_logger::OutputFormat::Pretty,
    };

    println!(">>> 正在以 {:?} 格式初始化 owl-logger ...\n", format);

    // 1. 初始化带多种高级配置的日志系统
    let _guard = owl_logger::builder()
        .file_name("adv_app")
        .log_dir("logs_advanced")
        .format(format)
        .global_field("env", "production")
        .global_field("version", "1.2.3")
        .sensitive_key("api_key") // 追加一个敏感字段
        .retention_days(7) // 仅保留 7 天内的日志
        .buffered_lines_limit(500_000) // 增大并发缓冲区
        .lossy(false) // 队列满时不丢失，选择阻塞（高并发下防丢失）
        .init();

    tracing::info!("🦉 日志系统初始化完成，已开启各项高级生产特性");

    // 2. 验证敏感字段脱敏
    // 默认敏感词有 password, token, secret, authorization, credit_card 等
    tracing::warn!(
        password = "my-secret-password-123",
        api_key = "sk-live-xyz987abc",
        user = "alice",
        "用户尝试登录授权"
    );

    // 3. 验证 #[monitor] 宏的 Result 检测与级别自动升级
    println!("\n>>> 调用成功行动（应以正常级别输出）...");
    let _ = perform_success_action("alice");

    println!("\n>>> 调用失败行动（Err 返回值，应自动以 ERROR 级别输出且带错误详情）...");
    let _ = perform_error_action("bob");
}
