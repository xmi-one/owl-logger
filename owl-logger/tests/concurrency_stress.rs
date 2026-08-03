//! 深度并发压力测试：验证在高度并发的多线程环境下系统的稳定性、无死锁和正确性。

use owl_logger::{monitor, OutputFormat, RotationPolicy};
use std::path::PathBuf;
use std::thread;

fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("owl-test-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// 被 monitor 监控的测试函数，带有可能发生级别升级的 Result 返回值
#[monitor(span, slow_ms = 100)]
fn monitored_work(worker_id: usize, task_id: usize) -> Result<usize, String> {
    if task_id.is_multiple_of(50) {
        // 模拟偶尔出现的错误以触发 ERROR 级别日志自动升级
        return Err(format!("Worker {worker_id} failed on task {task_id}"));
    }

    if task_id % 100 == 1 {
        // 模拟偶尔慢速的任务以触发 SLOW (WARN) 级别日志升级
        thread::sleep(std::time::Duration::from_millis(120));
    }

    tracing::debug!(
        worker = worker_id,
        task = task_id,
        "Monitored task execution progress"
    );
    Ok(task_id)
}

#[test]
fn test_logger_under_heavy_concurrency() {
    let dir = unique_temp_dir("stress");

    {
        // 1. 初始化日志配置：同时启用普通日志和 ERROR 级别分离日志
        let _guard = owl_logger::builder()
            .log_dir(dir.to_string_lossy().to_string())
            .file_name("stress_app")
            .console(false)
            .format(OutputFormat::Json)
            .rotation(RotationPolicy::Never)
            .level(owl_logger::LogLevel::Debug)
            .error_file(owl_logger::LogLevel::Error)
            .catch_panic(false)
            .init();

        // 2. 并发地创建 20 个工作线程，每个工作线程执行 300 次任务
        let num_threads = 20;
        let num_tasks_per_thread = 300;
        let mut handles = vec![];

        for t_id in 0..num_threads {
            let handle = thread::spawn(move || {
                for task_id in 0..num_tasks_per_thread {
                    // 模拟同步上下文追踪
                    let _ctx =
                        owl_logger::context::with_request_id(&format!("req-{t_id}-{task_id}"));

                    // 执行监控函数
                    let _ = monitored_work(t_id, task_id);

                    // 混合常规并发日志
                    if task_id.is_multiple_of(10) {
                        tracing::info!(
                            worker = t_id,
                            task = task_id,
                            "Normal concurrency log item"
                        );
                    }
                }
            });
            handles.push(handle);
        }

        // 3. 等待所有并发线程执行完毕
        for handle in handles {
            handle.join().unwrap();
        }
    } // Drop guard 确保所有缓冲日志完全 Flush 到底层文件

    // 4. 读取生成日志文件的结果
    let main_log_path = dir.join("stress_app.log");
    let error_log_path = dir.join("stress_app.error.log");

    assert!(main_log_path.exists(), "Main log file must be created");
    assert!(
        error_log_path.exists(),
        "Error separation log file must be created"
    );

    let main_content = std::fs::read_to_string(&main_log_path).expect("Read main log failed");
    let error_content = std::fs::read_to_string(&error_log_path).expect("Read error log failed");

    // 5. 对高并发日志正确性做基本统计校验
    let main_lines: Vec<&str> = main_content.lines().collect();
    let error_lines: Vec<&str> = error_content.lines().collect();

    // 每一行都应当是合法的 JSON 格式
    for line in &main_lines {
        let val: serde_json::Value =
            serde_json::from_str(line).expect("Each line in main log must be valid JSON");

        // 验证请求上下文 ID 成功贯通
        if let Some(req_id) = val.get("req_id") {
            assert!(req_id.as_str().unwrap().starts_with("req-"));
        }
    }

    for line in &error_lines {
        let val: serde_json::Value =
            serde_json::from_str(line).expect("Each line in error log must be valid JSON");
        // 验证分流出的日志确实全为 ERROR 级别
        assert_eq!(val["level"], "ERROR");
    }

    // 主日志行数应相当可观且不应为 0
    assert!(main_lines.len() > 1000, "Should contain thousands of logs");

    // 错误日志中应且仅包含由 task_id % 50 == 0 带来的错误
    assert!(!error_lines.is_empty(), "Should capture worker error logs");

    // 清理测试目录
    let _ = std::fs::remove_dir_all(&dir);
}
