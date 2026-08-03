//! 演示运行时动态调整日志级别与过滤规则
//!
//! 运行：`cargo run --example dynamic_level -p owl-logger`

fn main() {
    // 1. 初始化，默认级别为 Info
    let _guard = owl_logger::builder()
        .level(owl_logger::LogLevel::Info)
        .init();

    tracing::info!("🦉 日志库初始化完成，当前默认为 Info 级别");
    tracing::debug!("这是一条 DEBUG 日志，目前应该看不见...");

    // 2. 动态修改全局级别为 Debug
    println!("\n>>> 动态将级别修改为 DEBUG ...\n");
    owl_logger::set_level(owl_logger::LogLevel::Debug).unwrap();

    tracing::debug!("🦉 成功！现在可以看到 DEBUG 级别的日志了！");
    tracing::trace!("这是一条 TRACE 日志，目前应该看不见...");

    // 3. 动态设置复杂的过滤规则（例如：允许本模块的 TRACE 级别）
    println!("\n>>> 动态设置更高级的过滤规则: 'dynamic_level=trace' ...\n");
    owl_logger::set_filter("dynamic_level=trace").unwrap();

    tracing::trace!("🦉 成功！现在连 TRACE 日志也可以看到了！");

    // 4. 查询当前过滤规则
    let current_filter = owl_logger::get_filter().unwrap();
    println!("\n>>> 当前过滤规则为: {current_filter}");
}
