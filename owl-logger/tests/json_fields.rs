//! 集成测试：JSON 输出格式会原样保留事件字段值。

use owl_logger::{OutputFormat, RotationPolicy};
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
fn json_output_preserves_event_field_values() {
    let dir = unique_temp_dir("json");

    {
        let _guard = owl_logger::builder()
            .log_dir(dir.to_string_lossy().to_string())
            .file_name("app")
            .console(false)
            .format(OutputFormat::Json)
            .rotation(RotationPolicy::Never)
            .catch_panic(false)
            .init();

        tracing::warn!(
            password = "super-secret",
            user = "bob",
            attempts = 3,
            "login attempt"
        );
    } // drop guard -> flush

    let content = std::fs::read_to_string(dir.join("app.log")).expect("log must exist");

    let line = content
        .lines()
        .find(|l| l.contains("login attempt"))
        .expect("must find the logged line");

    let value: serde_json::Value = serde_json::from_str(line).expect("line must be valid JSON");

    assert_eq!(value["message"], "login attempt");
    assert_eq!(value["level"], "WARN");
    assert_eq!(
        value["password"], "super-secret",
        "event field values must not be changed"
    );
    assert_eq!(value["user"], "bob");
    assert_eq!(value["attempts"], 3);

    let _ = std::fs::remove_dir_all(&dir);
}
