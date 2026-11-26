//! 集成测试：验证 `#[monitor]` 宏的返回类型反向推导、参数/返回值截断保护，
//! 以及 `#[monitor(span)]` 在同步/异步函数中的进入/退出/异常日志均归属于自身 Span。

use owl_logger::{monitor, LogLevel, RotationPolicy};
use std::path::PathBuf;

fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("owl-test-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[monitor]
fn collect_vec() -> Vec<i32> {
    (0..3).collect()
}

#[monitor]
fn ret_into() -> String {
    "inferred-string".into()
}

#[monitor]
fn ret_default() -> i32 {
    Default::default()
}

#[monitor]
fn ret_impl_debug() -> impl std::fmt::Debug {
    42usize
}

#[monitor]
fn ret_borrowed(input: &str) -> &str {
    input
}

#[monitor(span, max_len = 16)]
fn sync_failing_span(payload: &str) -> Result<(), String> {
    let _ = payload;
    Err("sync-span-failure-detail-long".to_string())
}

#[monitor(span, max_len = 16)]
async fn async_failing_span(payload: &str) -> Result<(), String> {
    tokio::task::yield_now().await;
    let _ = payload;
    Err("async-span-failure-detail-long".to_string())
}

#[tokio::test]
async fn monitor_supports_type_inference_truncation_and_span_scoping() {
    let dir = unique_temp_dir("monitor-inference-span");

    {
        let guard = owl_logger::builder()
            .log_dir(dir.to_string_lossy().to_string())
            .file_name("app")
            .console(false)
            .level(LogLevel::Info)
            .rotation(RotationPolicy::Never)
            .catch_panic(false)
            .init();

        assert_eq!(collect_vec(), vec![0, 1, 2]);
        assert_eq!(ret_into(), "inferred-string");
        assert_eq!(ret_default(), 0);
        let _ = ret_impl_debug();
        assert_eq!(ret_borrowed("borrowed"), "borrowed");

        let _ = sync_failing_span("abcdefghijklmnopqrstuvwxyz0123456789");
        let _ = async_failing_span("abcdefghijklmnopqrstuvwxyz0123456789").await;

        guard.flush().expect("runtime flush must succeed");
    }

    let content = std::fs::read_to_string(dir.join("app.log")).expect("log must exist");

    let sync_err_line = content
        .lines()
        .find(|line| line.contains("exiting sync_failing_span") && line.contains("ERROR:"))
        .expect("sync error exit log must exist");
    assert!(
        sync_err_line.contains("sync_failing_span") && sync_err_line.contains("..."),
        "sync monitor(span) exit log must carry span and truncate long payload: {sync_err_line}"
    );
    // 验证 span 名称出现在分隔列中（即事件确实发生在该 span 作用域内）
    assert!(
        sync_err_line.contains("| sync_failing_span |"),
        "sync exit log must execute inside sync_failing_span scope: {sync_err_line}"
    );

    let async_err_line = content
        .lines()
        .find(|line| line.contains("exiting async_failing_span") && line.contains("ERROR:"))
        .expect("async error exit log must exist");
    assert!(
        async_err_line.contains("| async_failing_span |"),
        "async exit log must execute inside async_failing_span scope: {async_err_line}"
    );
    assert!(
        async_err_line.contains("..."),
        "async monitor max_len truncation must apply: {async_err_line}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
