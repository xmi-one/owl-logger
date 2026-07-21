//! 集成测试：事件、span 和全局字段重名时均不能丢失。

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
fn json_output_preserves_event_span_and_global_collisions() {
    let dir = unique_temp_dir("json-cross-collisions");

    {
        let _guard = owl_logger::builder()
            .log_dir(dir.to_string_lossy().to_string())
            .file_name("app")
            .console(false)
            .format(OutputFormat::Json)
            .rotation(RotationPolicy::Never)
            .global_field("region", "global")
            .catch_panic(false)
            .init();

        let span = tracing::info_span!("request", region = "span");
        let _entered = span.enter();
        tracing::info!(region = "event", "collision check");
    }

    let content = std::fs::read_to_string(dir.join("app.log")).expect("log must exist");
    let line = content
        .lines()
        .find(|line| line.contains("collision check"))
        .expect("event must be present");
    let value: serde_json::Value = serde_json::from_str(line).expect("line must be valid JSON");

    assert_eq!(value["region"], "event");
    assert_eq!(value["_region"], "span");
    assert_eq!(value["__region"], "global");

    let _ = std::fs::remove_dir_all(&dir);
}
