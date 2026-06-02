//! 集成测试：请求上下文（req_id span）字段在输出中正确出现，
//! 验证结构化 span 字段收集层（OwlSpanLayer）端到端工作。

use owl_logger::RotationPolicy;
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
fn request_id_span_field_appears_in_output() {
    let dir = unique_temp_dir("span");

    {
        let _guard = owl_logger::builder()
            .log_dir(dir.to_string_lossy().to_string())
            .file_name("app")
            .console(false)
            .rotation(RotationPolicy::Never)
            .catch_panic(false)
            .init();

        {
            let _ctx = owl_logger::context::with_request_id("req-xyz");
            tracing::info!("inside context");
        }
        tracing::info!("outside context");
    } // drop guard -> flush

    let content = std::fs::read_to_string(dir.join("app.log")).expect("log must exist");

    let inside = content
        .lines()
        .find(|l| l.contains("inside context"))
        .expect("must find the in-context line");
    let outside = content
        .lines()
        .find(|l| l.contains("outside context"))
        .expect("must find the out-of-context line");

    // 上下文内的日志带 req_id span 字段
    assert!(
        inside.contains("request") && inside.contains("req_id=\"req-xyz\""),
        "in-context line should carry req_id: {inside}"
    );
    // 上下文外的日志不应带 req_id
    assert!(
        !outside.contains("req_id=\"req-xyz\""),
        "out-of-context line should not carry req_id: {outside}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
