//! 第二轮加固集成测试：
//! 1. `#[monitor]` 支持 `&mut self -> &mut T` / `&mut [T] -> Result<&mut T, E>` 借用逃逸、`skip(self)`、`module_target` 与自定义 `target`；
//! 2. 纯模块作用域过滤器（如 `round2_hardening=info`）下自动保留 `with_request_id` 请求上下文 Span；
//! 3. `.process_isolation(true)` 生成带当前进程 PID 的隔离主日志与错误日志文件；
//! 4. 文本格式 `%` Display 字段防键值注入，且多段带引号消息不误吞首尾引号；
//! 5. 同一条 `ERROR` 事件在主日志与错误日志中的纳秒级时间戳严格一致，且相邻事件正常推进。

use owl_logger::{monitor, LogLevel, OutputFormat, RotationPolicy};
use std::path::PathBuf;
use std::time::Duration;

/// 故意不实现 `Debug`，验证 `#[monitor(skip(self))]` 不会对 `self` 生成格式化约束。
struct SecretVault {
    token: String,
}

impl SecretVault {
    #[monitor(skip(self), module_target)]
    fn token_mut(&mut self, prefix: &str) -> &mut String {
        self.token.insert_str(0, prefix);
        &mut self.token
    }
}

#[monitor(target = "audit_mon")]
fn head_mut(items: &mut [String]) -> Result<&mut String, &'static str> {
    items.first_mut().ok_or("empty slice")
}

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
fn round2_macro_borrows_scoped_filter_process_isolation_injection_and_timestamp_sync() {
    let dir = unique_temp_dir("round2-hardening");
    let pid = std::process::id();

    let guard = owl_logger::builder()
        .log_dir(dir.to_string_lossy().to_string())
        .file_name("svc")
        .process_isolation(true)
        .console(false)
        .file_format(OutputFormat::Pretty)
        .error_file(LogLevel::Error)
        .time_format("%Y-%m-%d %H:%M:%S%.9f")
        .rotation(RotationPolicy::Never)
        .catch_panic(false)
        .init();

    // 切换为纯模块级过滤器（不含全局裸级别），验证上下文 Span 仍保留且无关模块被过滤
    owl_logger::set_filter("round2_hardening=info,audit_mon=info")
        .expect("module-scoped filter reload must succeed");

    {
        let _req = owl_logger::context::with_request_id("req-scoped-99");

        // 无关模块日志应被屏蔽
        tracing::info!(target: "noisy_third_party", "must_be_filtered_out");

        // 1. 验证 `&mut self -> &mut String` + `skip(self)` + `module_target`
        let mut vault = SecretVault {
            token: "core".to_string(),
        };
        let tok = vault.token_mut("v2:");
        tok.push_str(":ok");
        assert_eq!(vault.token, "v2:core:ok");

        // 2. 验证 `&mut [String] -> Result<&mut String, _>` 在 Ok 与 Err 分支下均可正常工作
        let mut items = vec!["alpha".to_string()];
        let first = head_mut(&mut items).unwrap();
        first.push_str("-mutated");
        assert_eq!(items[0], "alpha-mutated");

        let mut empty: Vec<String> = Vec::new();
        assert_eq!(head_mut(&mut empty), Err("empty slice"));

        // 3. 验证 `%` Display 恶意输入防字段注入 & 多段引号消息不被剥掉首尾引号
        let attacker_input = "\"guest\" role=\"admin\"";
        tracing::error!(
            user = %attacker_input,
            "\"quoted\" and \"more\""
        );

        std::thread::sleep(Duration::from_millis(2));
        tracing::error!("second_error_after_sleep");
    }

    owl_logger::flush().expect("flush must succeed");
    assert_eq!(guard.sink_errors().total(), 0);
    drop(guard);

    let main_path = dir.join(format!("svc.pid{pid}.log"));
    let err_path = dir.join(format!("svc.pid{pid}.error.log"));
    assert!(
        main_path.exists(),
        "process_isolation must create {main_path:?}"
    );
    assert!(
        err_path.exists(),
        "process_isolation must create {err_path:?}"
    );

    let main_content = std::fs::read_to_string(&main_path).unwrap();
    let err_content = std::fs::read_to_string(&err_path).unwrap();

    // 无关模块已被过滤
    assert!(!main_content.contains("must_be_filtered_out"));

    // 模块级过滤器下 `with_request_id` 的 `req_id` 仍然完整保留
    let vault_line = main_content
        .lines()
        .find(|l| l.contains("token_mut"))
        .expect("module_target monitor log must appear under round2_hardening=info");
    assert!(
        vault_line.contains("req_id=\"req-scoped-99\""),
        "request context must survive module-scoped filter: {vault_line}"
    );

    // `audit_mon` 自定义 target 的 Err 自动升级为 ERROR 并进入 error.log
    let head_err_line = err_content
        .lines()
        .find(|l| l.contains("head_mut") && l.contains("empty slice"))
        .expect("Err return on custom target must be promoted to ERROR and reach error.log");
    assert!(head_err_line.contains("req_id=\"req-scoped-99\""));

    // 验证字段注入已被转义阻断，且多段引号消息首尾引号均被保留转义
    let main_inj_line = main_content
        .lines()
        .find(|l| l.contains("quoted"))
        .expect("injection test line must exist in main log");
    let err_inj_line = err_content
        .lines()
        .find(|l| l.contains("quoted"))
        .expect("injection test line must exist in error log");

    assert!(
        !main_inj_line.contains("user=\"guest\" role=\"admin\""),
        "unescaped field injection must never occur: {main_inj_line}"
    );
    assert!(
        main_inj_line.contains("user=\"\\\"guest\\\" role=\\\"admin\\\"\""),
        "interior quotes must be escaped inside single quoted value: {main_inj_line}"
    );
    assert!(
        main_inj_line.contains("\\\"quoted\\\" and \\\"more\\\""),
        "multi-segment quoted message must keep outer quotes: {main_inj_line}"
    );

    // 验证同一条 ERROR 事件在主日志与错误日志中的纳秒级时间戳完全一致
    let main_ts = main_inj_line.split(" | ").next().unwrap();
    let err_ts = err_inj_line.split(" | ").next().unwrap();
    assert_eq!(
        main_ts, err_ts,
        "nanosecond timestamp for the exact same event must match across all sinks"
    );

    // 验证后续事件的时间戳正常向前推进（未被陈旧栈地址缓存冻结）
    let second_err_line = err_content
        .lines()
        .find(|l| l.contains("second_error_after_sleep"))
        .expect("second error line must exist");
    let second_ts = second_err_line.split(" | ").next().unwrap();
    assert_ne!(
        main_ts, second_ts,
        "subsequent events must capture a fresh timestamp"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
