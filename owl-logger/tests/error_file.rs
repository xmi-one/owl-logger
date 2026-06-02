//! 集成测试：按级别分文件（error_file）端到端行为。
//!
//! 全局 subscriber 每个进程只能初始化一次，因此本测试独占一个测试二进制。

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
fn error_file_only_captures_error_and_above() {
    let dir = unique_temp_dir("errfile");

    {
        let _guard = owl_logger::builder()
            .log_dir(dir.to_string_lossy().to_string())
            .file_name("app")
            .console(false)
            .rotation(RotationPolicy::Never)
            .error_file(LogLevel::Error)
            .catch_panic(false)
            .init();

        tracing::info!("plain info line");
        tracing::warn!("a warning line");
        tracing::error!("boom error line");
    } // drop guard -> flush

    let main = std::fs::read_to_string(dir.join("app.log")).expect("main log must exist");
    let err = std::fs::read_to_string(dir.join("app.error.log")).expect("error log must exist");

    // 主文件包含所有级别
    assert!(main.contains("plain info line"), "main log missing info");
    assert!(main.contains("boom error line"), "main log missing error");

    // 分级文件仅包含 ERROR
    assert!(err.contains("boom error line"), "error log missing error");
    assert!(
        !err.contains("plain info line"),
        "error log must not contain info: {err}"
    );
    assert!(
        !err.contains("a warning line"),
        "error log must not contain warn when threshold is Error: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
