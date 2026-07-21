//! 集成测试：只保留 ERROR 时，请求上下文仍需进入输出。

use owl_logger::{LogLevel, RotationPolicy};
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

#[test]
fn request_context_is_retained_when_only_errors_are_enabled() {
    let dir = unique_temp_dir("error-context");

    {
        let _guard = owl_logger::builder()
            .log_dir(dir.to_string_lossy().to_string())
            .file_name("app")
            .console(false)
            .level(LogLevel::Error)
            .rotation(RotationPolicy::Never)
            .catch_panic(false)
            .init();

        let _context = owl_logger::context::with_request_id("req-error-only");
        tracing::error!("request failed");
    }

    let content = std::fs::read_to_string(dir.join("app.log")).expect("log must exist");
    let line = content
        .lines()
        .find(|line| line.contains("request failed"))
        .expect("error log must be written");
    assert!(line.contains("req_id=\"req-error-only\""), "{line}");

    let _ = std::fs::remove_dir_all(&dir);
}
