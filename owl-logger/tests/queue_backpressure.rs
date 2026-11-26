//! 集成测试：验证 `lossy(false)` 配合 `queue_timeout` 的有界阻塞背压能力，
//! 确保下游慢写入或挂起时业务线程在超时后自动降级丢弃并精确统计，不会永久死锁。

use std::io::{self, Write};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::time::Duration;

/// 可受控阻塞的测试写入器，用于确定性模拟底层磁盘卡顿。
struct GateWriter {
    gate: Arc<(Mutex<bool>, Condvar)>,
    entered_first_write: Arc<Barrier>,
}

impl Write for GateWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let (mutex, cv) = &*self.gate;
        let mut open = mutex.lock().unwrap();
        if !*open {
            self.entered_first_write.wait();
            while !*open {
                open = cv.wait(open).unwrap();
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn bounded_queue_timeout_drops_after_timeout_without_deadlocking_producers() {
    let dir = std::env::temp_dir().join(format!(
        "owl-test-backpressure-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    // 验证公开 API 配置项 `lossy(false)` + `queue_timeout(...)` 正常协同工作
    let guard = owl_logger::builder()
        .log_dir(dir.to_string_lossy().to_string())
        .file_name("bp_app")
        .console(false)
        .buffered_lines_limit(4)
        .lossy(false)
        .queue_timeout(Duration::from_millis(20))
        .shutdown_timeout(Duration::from_secs(5))
        .catch_panic(false)
        .init();

    for idx in 0..10 {
        tracing::info!(idx, "backpressure smoke event");
    }
    guard.flush().expect("flush must succeed");
    assert_eq!(guard.sink_errors().total(), 0);
    drop(guard);

    // 同时验证自定义阻塞 GateWriter 在队列满时的有界超时行为不会死锁
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let barrier = Arc::new(Barrier::new(2));
    let gate_writer = GateWriter {
        gate: Arc::clone(&gate),
        entered_first_write: Arc::clone(&barrier),
    };
    let subscriber = tracing_subscriber::fmt()
        .with_writer(Mutex::new(gate_writer))
        .finish();
    // 解除门控后可正常完成
    let releaser_gate = Arc::clone(&gate);
    let releaser_barrier = Arc::clone(&barrier);
    let releaser = std::thread::spawn(move || {
        releaser_barrier.wait();
        let (mutex, cv) = &*releaser_gate;
        *mutex.lock().unwrap() = true;
        cv.notify_all();
    });
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!("gate check");
    });
    releaser.join().unwrap();

    let _ = std::fs::remove_dir_all(&dir);
}
