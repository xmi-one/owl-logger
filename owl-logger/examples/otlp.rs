//! 演示通过 OTLP/HTTP 将追踪 span 导出到 OpenTelemetry Collector / Jaeger / Tempo。
//!
//! 需要启用 `otlp` feature：
//! ```bash
//! # 先启动一个 OTLP 接收端，例如：
//! #   docker run --rm -p 4318:4318 -p 16686:16686 jaegertracing/all-in-one
//! cargo run --example otlp -p owl-logger --features otlp
//! ```
//!
//! 随后在 Jaeger UI (http://localhost:16686) 中即可看到名为 `owl-demo` 的服务追踪。

use owl_logger::monitor;

#[monitor(span)]
fn handle_order(order_id: u64) -> Result<String, String> {
    validate_payment(order_id)?;
    Ok(format!("order {order_id} confirmed"))
}

#[monitor(span)]
fn validate_payment(order_id: u64) -> Result<(), String> {
    std::thread::sleep(std::time::Duration::from_millis(20));
    tracing::info!(order_id, "支付校验通过");
    Ok(())
}

fn main() {
    let _guard = owl_logger::builder()
        .file_name("owl_otlp_demo")
        .log_dir("logs_otlp")
        .otlp_endpoint("http://localhost:4318/v1/traces")
        .otlp_service_name("owl-demo")
        .init();

    tracing::info!("🦉 OTLP 示例启动，开始处理订单");

    for id in 1..=3 {
        let _ = handle_order(id);
    }

    // 给后台批量导出器一点时间发送（Drop guard 时也会强制 flush 并 shutdown）
    std::thread::sleep(std::time::Duration::from_millis(500));
    tracing::info!("🦉 处理完成，正在退出（将 flush 并关闭 OTLP 导出）");
}
