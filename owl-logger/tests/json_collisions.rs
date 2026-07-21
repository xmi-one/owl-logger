//! 集成测试：验证 JSON 格式化输出中，保留关键字不被 Span、全局或事件自定义字段冲突覆盖。

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
fn json_output_protects_reserved_keys_from_collisions() {
    let dir = unique_temp_dir("collisions");

    {
        let _guard = owl_logger::builder()
            .log_dir(dir.to_string_lossy().to_string())
            .file_name("app")
            .console(false)
            .format(OutputFormat::Json)
            .rotation(RotationPolicy::Never)
            .global_field("message", "global_msg_attempt") // 全局字段冲突尝试
            .global_field("level", "global_lvl_attempt") // 全局字段冲突尝试
            .catch_panic(false)
            .init();

        // 创建带有冲突字段的 Span，并在其中发送带有冲突字段的日志
        let span = tracing::info_span!(
            "my_span",
            message = "span_msg_attempt",
            level = "span_lvl_attempt"
        );
        let _enter = span.enter();

        tracing::info!(
            timestamp = "event_ts_attempt",
            level = "event_lvl_attempt",
            "real_message_content"
        );
    } // drop guard -> flush

    let content = std::fs::read_to_string(dir.join("app.log")).expect("log must exist");

    // 找到包含我们消息的那一行并解析为 JSON
    let line = content
        .lines()
        .find(|l| l.contains("real_message_content"))
        .expect("must find the logged line");

    let value: serde_json::Value = serde_json::from_str(line).expect("line must be valid JSON");

    // 验证标准保留字段未被覆盖，保留其正确性质/类型
    assert_eq!(value["message"], "real_message_content");
    assert_eq!(value["level"], "INFO");
    assert!(value["timestamp"].is_string());
    assert_ne!(value["timestamp"], "event_ts_attempt");

    // 验证冲突的自定义字段已被添加了下划线前缀，没有丢失数据
    // 注意：事件字段最后写入，因此事件中的同名冲突字段最终会在 log_obj 中生效
    assert!(
        value["_message"].is_string(),
        "colliding message key must be prefixed"
    );
    assert!(
        value["_level"].is_string(),
        "colliding level key must be prefixed"
    );
    assert_eq!(
        value["_timestamp"], "event_ts_attempt",
        "colliding timestamp key must be prefixed"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
