//! 集成测试：自定义 subscriber 与动态过滤器必须可以共存并可靠重载。

use tracing_subscriber::prelude::*;

#[test]
fn custom_subscriber_keeps_dynamic_filter_reload_available() {
    // 在实际应用中，调用方常会先叠加自己的 Layer；这里用 LevelFilter 模拟该组合，
    // 以覆盖被类型擦除的 reload handle。
    let subscriber =
        tracing_subscriber::registry().with(tracing_subscriber::filter::LevelFilter::TRACE);
    let _guard = owl_logger::builder()
        .console(false)
        .file(false)
        .catch_panic(false)
        .try_init_with_subscriber(subscriber)
        .expect("custom subscriber initialization must succeed");

    let initial = owl_logger::get_filter().expect("filter handle must be available");
    assert!(
        initial.contains("info"),
        "unexpected initial filter: {initial}"
    );

    owl_logger::set_filter("info,monitor=warn").expect("filter reload must succeed");
    let reloaded = owl_logger::get_filter().expect("reloaded filter must be readable");
    assert!(
        reloaded.contains("monitor=warn"),
        "unexpected reloaded filter: {reloaded}"
    );

    owl_logger::set_level(owl_logger::LogLevel::Error).expect("level reload must succeed");
    let error_only = owl_logger::get_filter().expect("level filter must be readable");
    assert!(
        error_only.contains("error"),
        "unexpected level filter: {error_only}"
    );
}
