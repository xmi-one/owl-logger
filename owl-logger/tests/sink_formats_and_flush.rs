//! 集成测试：验证独立通道格式/级别、运行期主动 `flush()`、嵌套 Span 优先级、
//! `context_span!` 原生数值保真，以及二次 `try_init()` 零磁盘副作用。

use owl_logger::{LogLevel, OutputFormat, OwlError, RotationPolicy};
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
fn per_sink_format_level_runtime_flush_and_side_effect_free_reinit() {
    let dir = unique_temp_dir("sink-flush");
    let untouched_dir = dir.join("must_not_be_created_on_failed_reinit");

    // 验证文件不可打开（同名目录占位）时启动期立即返回 FileAppenderCreation
    let blocked_dir = dir.join("blocked");
    std::fs::create_dir_all(blocked_dir.join("bad.log")).unwrap();
    let fail_fast = owl_logger::builder()
        .log_dir(blocked_dir.to_string_lossy().to_string())
        .file_name("bad")
        .console(false)
        .try_init();
    assert!(
        matches!(fail_fast, Err(OwlError::FileAppenderCreation(_))),
        "unopenable log file must fail fast at init"
    );

    let guard = owl_logger::builder()
        .log_dir(dir.to_string_lossy().to_string())
        .file_name("app")
        .console(false)
        .console_format(OutputFormat::Pretty)
        .file_format(OutputFormat::Json)
        .console_level(LogLevel::Warn)
        .file_level(LogLevel::Debug)
        .rotation(RotationPolicy::Never)
        .catch_panic(false)
        .init();

    // 二次 try_init 必须在创建目录或清理文件之前快速返回 AlreadyInitialized
    let second = owl_logger::builder()
        .log_dir(untouched_dir.to_string_lossy().to_string())
        .file_name("stray")
        .try_init();
    assert!(matches!(second, Err(OwlError::AlreadyInitialized)));
    assert!(
        !untouched_dir.exists(),
        "failed try_init must not create stray log directories"
    );

    {
        let outer = owl_logger::context_span!("outer", step = "outer", attempt = 3, ok = true);
        let _outer_guard = outer.enter();
        let inner = tracing::debug_span!("inner", step = "inner");
        let _inner_guard = inner.enter();

        tracing::debug!("debug event inside nested spans");
    }

    // 在不销毁 guard 的前提下主动 flush，文件内容应立即可读
    owl_logger::flush().expect("global flush must succeed");
    assert_eq!(guard.sink_errors().total(), 0);

    let content =
        std::fs::read_to_string(dir.join("app.log")).expect("flushed log must exist while live");
    let debug_line = content
        .lines()
        .find(|line| line.contains("debug event inside nested spans"))
        .expect("file_level=Debug must capture debug event");
    let json: serde_json::Value =
        serde_json::from_str(debug_line).expect("file_format=Json must produce valid JSON");

    // 内层活跃子 Span 优先保留 `step`，外层父 Span 变为 `_step`
    assert_eq!(json["step"], "inner");
    assert_eq!(json["_step"], "outer");
    // context_span! 保留原始数值与布尔类型
    assert_eq!(json["attempt"], 3);
    assert_eq!(json["ok"], true);

    drop(guard);
    let _ = std::fs::remove_dir_all(&dir);
}
