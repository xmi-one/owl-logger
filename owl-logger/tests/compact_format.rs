//! 集成测试：Compact 格式会保留事件字段并附加全局字段。

use owl_logger::{LogLevel, OutputFormat, RotationPolicy};
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
fn compact_output_preserves_event_fields_and_includes_global_fields_in_all_files() {
    let dir = unique_temp_dir("compact");

    {
        let guard = owl_logger::builder()
            .log_dir(dir.to_string_lossy().to_string())
            .file_name("app")
            .console(false)
            .format(OutputFormat::Compact)
            .rotation(RotationPolicy::Never)
            .global_field("environment", "production")
            .error_file(LogLevel::Error)
            .catch_panic(false)
            .init();

        tracing::info!(api_key = "main-file-secret", "main compact event");
        tracing::info!(
            request = "first line\nsecond line",
            "message line 1\nmessage line 2"
        );
        tracing::error!(api_key = "error-file-secret", "error compact event");
        assert_eq!(guard.dropped_lines().total(), 0);
    }

    let main = std::fs::read_to_string(dir.join("app.log")).expect("main log must exist");
    let error = std::fs::read_to_string(dir.join("app.error.log")).expect("error log must exist");

    assert!(main.contains("main compact event"));
    assert!(main.contains("api_key=\"main-file-secret\""));
    assert!(main.contains("environment=\"production\""));
    assert!(main.contains("error-file-secret"));
    assert!(main.contains("message line 1\\nmessage line 2"));
    assert!(main.contains("request=\"first line\\nsecond line\""));
    assert!(!main.contains("message line 1\nmessage line 2"));

    assert!(error.contains("error compact event"));
    assert!(error.contains("api_key=\"error-file-secret\""));
    assert!(error.contains("environment=\"production\""));

    let _ = std::fs::remove_dir_all(&dir);
}
