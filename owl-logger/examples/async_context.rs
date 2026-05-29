//! 异步上下文追踪示例
//!
//! 运行：`cargo run --example async_context -p owl-logger`

use owl_logger::{Language, LogLevel};
use tracing::Instrument;

#[tokio::main]
async fn main() {
    let _guard = owl_logger::builder()
        .file_name("async_example")
        .log_dir("logs")
        .language(Language::Zh)
        .level(LogLevel::Debug)
        .show_target(true)
        .init();

    owl_logger::info!("异步上下文追踪示例启动");

    // 模拟并发处理多个请求
    let handles: Vec<_> = (1..=3)
        .map(|i| {
            let req_id = format!("req-{i:03}");
            tokio::spawn(
                async move {
                    handle_request(i).await;
                }
                .instrument(owl_logger::context::request_span(&req_id)),
            )
        })
        .collect();

    for handle in handles {
        handle.await.unwrap();
    }

    owl_logger::info!("所有请求处理完成");
}

async fn handle_request(id: u32) {
    owl_logger::info!(request_id = id, "开始处理请求");

    // 模拟异步操作
    tokio::time::sleep(std::time::Duration::from_millis(100 * id as u64)).await;

    owl_logger::info!(request_id = id, "查询数据库完成");

    // 嵌套 Span — 使用 .instrument() 而非 .entered() 来兼容异步
    async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        owl_logger::debug!(rows = 42, "查询返回结果");
    }
    .instrument(owl_logger::info_span!("db_query", table = "orders"))
    .await;

    owl_logger::info!(request_id = id, "请求处理完成");
}
