//! 集成测试：验证流式 JSON 格式化器在极端数值（`u128::MAX`、`i128::MIN`、`NaN`）、
//! 无 `message` 事件、多段引号 `%` Display 字符串以及 `_key`/`key` 交错冲突下的精确性。

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
fn streaming_json_preserves_quotes_extreme_numbers_and_empty_messages() {
    let dir = unique_temp_dir("json-quote-integrity");

    {
        let _guard = owl_logger::builder()
            .log_dir(dir.to_string_lossy().to_string())
            .file_name("app")
            .console(false)
            .format(OutputFormat::Json)
            .rotation(RotationPolicy::Never)
            .catch_panic(false)
            .init();

        // 1. 多段带引号字符串（验证首尾引号绝不被误剥离）与极端 128 位整数/NaN
        let attacker = "\"guest\" role=\"admin\"";
        tracing::info!(
            user = %attacker,
            big_u = u128::MAX,
            big_i = i128::MIN,
            nan_val = f64::NAN,
            _target = "prefixed_first",
            target = "colliding_reserved",
            "\"quoted\" and \"more\""
        );

        // 2. 无 message 字段的结构化事件（验证 `"message": ""` 默认空串仍稳定输出）
        tracing::info!(metric_only = 42_u64, active = true);
    }

    let content = std::fs::read_to_string(dir.join("app.log")).expect("log must exist");

    let quoted_line = content
        .lines()
        .find(|l| l.contains("colliding_reserved"))
        .expect("quoted event line must exist");
    let v1: serde_json::Value = serde_json::from_str(quoted_line).expect("must be valid JSON");

    assert_eq!(v1["user"], "\"guest\" role=\"admin\"");
    assert_eq!(v1["message"], "\"quoted\" and \"more\"");
    assert_eq!(v1["big_u"], u128::MAX.to_string());
    assert_eq!(v1["big_i"], i128::MIN.to_string());
    assert_eq!(v1["nan_val"], "NaN");
    assert_eq!(v1["_target"], "prefixed_first");
    assert_eq!(v1["__target"], "colliding_reserved");

    let metric_line = content
        .lines()
        .find(|l| l.contains("\"metric_only\":42"))
        .expect("metric-only event line must exist");
    let v2: serde_json::Value = serde_json::from_str(metric_line).expect("must be valid JSON");
    assert_eq!(v2["message"], "");
    assert_eq!(v2["metric_only"], 42);
    assert_eq!(v2["active"], true);

    let _ = std::fs::remove_dir_all(&dir);
}
