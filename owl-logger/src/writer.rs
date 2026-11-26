use std::collections::VecDeque;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::Timelike;
use tracing_subscriber::fmt::MakeWriter;

use crate::config::RotationPolicy;
use crate::error::OwlError;

const BATCH_DRAIN_LIMIT: usize = 256;
const FILE_BUF_CAPACITY: usize = 64 * 1024;
const ORPHAN_STAGING_MIN_AGE: Duration = Duration::from_secs(3600);
const DEQUE_SHRINK_THRESHOLD: usize = 4096;
const DEQUE_SHRINK_TARGET: usize = 1024;
const PATH_CHECK_INTERVAL: Duration = Duration::from_millis(200);

/// 安全计算单调时钟截止点；当 `timeout` 极大（如 `Duration::MAX`）导致加法溢出时，
/// 平滑退化为 `None`（无上限等待），绝不在业务线程或 `Drop` 中触发溢出 panic。
#[inline]
pub(crate) fn compute_deadline(timeout: Option<Duration>) -> Option<Instant> {
    timeout.and_then(|dur| Instant::now().checked_add(dur))
}

/// 饱和递增的无锁原子计数器（用于统计丢弃行数与底层 I/O 错误次数）。
#[derive(Clone, Debug, Default)]
pub(crate) struct AtomicCounter(Arc<AtomicUsize>);

impl AtomicCounter {
    pub(crate) fn new() -> Self {
        Self(Arc::new(AtomicUsize::new(0)))
    }

    pub(crate) fn get(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }

    pub(crate) fn incr_saturating(&self) {
        let mut curr = self.0.load(Ordering::Acquire);
        if curr == usize::MAX {
            return;
        }
        loop {
            let next = curr.saturating_add(1);
            match self
                .0
                .compare_exchange_weak(curr, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(actual) => {
                    if actual == usize::MAX {
                        return;
                    }
                    curr = actual;
                }
            }
        }
    }
}

enum QueueItem {
    Line(Vec<u8>),
    Flush(u64),
    Shutdown,
}

struct QueueState {
    deque: VecDeque<QueueItem>,
    buffered_lines: usize,
    waiting_producers: usize,
    waiting_flushers: usize,
    stopped: bool,
    next_flush_seq: u64,
    flushed_seq: u64,
    worker_finished: bool,
    worker_thread_id: Option<ThreadId>,
}

pub(crate) struct AsyncQueueShared {
    mutex: Mutex<QueueState>,
    not_empty: Condvar,
    state_changed: Condvar,
    limit: usize,
    lossy: bool,
    queue_timeout: Option<Duration>,
    dropped_lines: AtomicCounter,
    sink_errors: AtomicCounter,
}

/// 异步批量日志写入器（参考 yyds-logger 的 `_ThreadQueue` 设计）。
///
/// 特性：
/// - 支持 `drain_batch(256)` 一次锁内批量出队，显著降低高并发锁竞争与系统调用；
/// - 跟踪 `waiting_producers` / `waiting_flushers`，稳态零无效 `Condvar` 唤醒，单槽位释放时 `notify_one` 防惊群；
/// - 流量洪峰退去后自动收缩超大容量 `VecDeque` 回收内存；
/// - 控制帧（`Flush` / `Shutdown`）不受容量上限阻塞，支持运行期主动 `flush()` 与 panic 紧急刷盘；
/// - 支持 `lossy(false)` 配合可选 `queue_timeout` 实现有界阻塞背压；
/// - 独立统计 `dropped_lines` 与底层写入/轮转/压缩 `sink_errors`。
#[derive(Clone)]
pub(crate) struct OwlAsyncWriter {
    shared: Arc<AsyncQueueShared>,
}

pub(crate) struct OwlAsyncWriterOptions {
    pub(crate) buffered_lines_limit: usize,
    pub(crate) lossy: bool,
    pub(crate) queue_timeout: Option<Duration>,
    pub(crate) shutdown_timeout: Option<Duration>,
    pub(crate) thread_name: String,
    pub(crate) sink_errors: Option<AtomicCounter>,
}

impl OwlAsyncWriter {
    pub(crate) fn spawn<W>(
        mut writer: W,
        options: OwlAsyncWriterOptions,
    ) -> (Self, OwlWorkerGuard, OwlFlushHandle)
    where
        W: Write + Send + 'static,
    {
        let sink_errors = options.sink_errors.unwrap_or_default();
        let shared = Arc::new(AsyncQueueShared {
            mutex: Mutex::new(QueueState {
                deque: VecDeque::new(),
                buffered_lines: 0,
                waiting_producers: 0,
                waiting_flushers: 0,
                stopped: false,
                next_flush_seq: 0,
                flushed_seq: 0,
                worker_finished: false,
                worker_thread_id: None,
            }),
            not_empty: Condvar::new(),
            state_changed: Condvar::new(),
            limit: options.buffered_lines_limit.max(1),
            lossy: options.lossy,
            queue_timeout: options.queue_timeout,
            dropped_lines: AtomicCounter::new(),
            sink_errors,
        });

        let worker_shared = Arc::clone(&shared);
        let join_handle = thread::Builder::new()
            .name(options.thread_name)
            .spawn(move || {
                {
                    let mut state = worker_shared
                        .mutex
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    state.worker_thread_id = Some(thread::current().id());
                }

                let mut batch = Vec::with_capacity(BATCH_DRAIN_LIMIT);
                let mut shutting_down = false;

                while !shutting_down {
                    let queue_empty_after_pop = {
                        let mut state = worker_shared
                            .mutex
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        while state.deque.is_empty() {
                            state = worker_shared
                                .not_empty
                                .wait(state)
                                .unwrap_or_else(|e| e.into_inner());
                        }

                        let mut lines_popped = 0usize;
                        while batch.len() < BATCH_DRAIN_LIMIT {
                            let Some(item) = state.deque.pop_front() else {
                                break;
                            };
                            if matches!(item, QueueItem::Line(_)) {
                                lines_popped += 1;
                            }
                            batch.push(item);
                        }

                        if lines_popped > 0 {
                            state.buffered_lines =
                                state.buffered_lines.saturating_sub(lines_popped);
                            if state.waiting_producers > 0 {
                                if state.waiting_producers == 1
                                    || (lines_popped == 1 && state.waiting_flushers == 0)
                                {
                                    worker_shared.state_changed.notify_one();
                                } else {
                                    worker_shared.state_changed.notify_all();
                                }
                            }
                        }

                        let is_empty = state.deque.is_empty();
                        if is_empty && state.deque.capacity() > DEQUE_SHRINK_THRESHOLD {
                            state.deque.shrink_to(DEQUE_SHRINK_TARGET);
                        }
                        is_empty
                    };

                    let mut flushed_in_batch = false;
                    for item in batch.drain(..) {
                        match item {
                            QueueItem::Line(bytes) => {
                                if writer.write_all(&bytes).is_err() {
                                    worker_shared.sink_errors.incr_saturating();
                                }
                                flushed_in_batch = false;
                            }
                            QueueItem::Flush(seq) => {
                                if writer.flush().is_err() {
                                    worker_shared.sink_errors.incr_saturating();
                                }
                                flushed_in_batch = true;
                                let mut state = worker_shared
                                    .mutex
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner());
                                if seq > state.flushed_seq {
                                    state.flushed_seq = seq;
                                }
                                if state.waiting_flushers > 0 {
                                    worker_shared.state_changed.notify_all();
                                }
                            }
                            QueueItem::Shutdown => {
                                shutting_down = true;
                                break;
                            }
                        }
                    }

                    if !shutting_down
                        && queue_empty_after_pop
                        && !flushed_in_batch
                        && writer.flush().is_err()
                    {
                        worker_shared.sink_errors.incr_saturating();
                    }
                }

                if writer.flush().is_err() {
                    worker_shared.sink_errors.incr_saturating();
                }
                // 先销毁底层 writer（触发文件关闭及后台压缩线程汇合），再置位 worker_finished，
                // 确保 OwlGuard Drop 返回时所有落盘与 Gzip 压缩已彻底完成。
                drop(writer);

                let mut state = worker_shared
                    .mutex
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                state.worker_finished = true;
                worker_shared.state_changed.notify_all();
            })
            .expect("owl-logger: failed to spawn async logging worker thread");

        let flush_handle = OwlFlushHandle {
            shared: Arc::downgrade(&shared),
            #[cfg(test)]
            default_timeout: options.shutdown_timeout,
        };
        let guard = OwlWorkerGuard {
            shared: Arc::clone(&shared),
            join_handle: Some(join_handle),
            shutdown_timeout: options.shutdown_timeout,
        };

        (Self { shared }, guard, flush_handle)
    }

    pub(crate) fn dropped_counter(&self) -> AtomicCounter {
        self.shared.dropped_lines.clone()
    }

    pub(crate) fn sink_error_counter(&self) -> AtomicCounter {
        self.shared.sink_errors.clone()
    }
}

impl Write for OwlAsyncWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = buf.len();
        let mut state = self.shared.mutex.lock().unwrap_or_else(|e| e.into_inner());

        if state.stopped {
            return Ok(len);
        }

        if state.buffered_lines >= self.shared.limit {
            if self.shared.lossy {
                self.shared.dropped_lines.incr_saturating();
                return Ok(len);
            }

            match compute_deadline(self.shared.queue_timeout) {
                None => {
                    state.waiting_producers += 1;
                    while !state.stopped && state.buffered_lines >= self.shared.limit {
                        state = self
                            .shared
                            .state_changed
                            .wait(state)
                            .unwrap_or_else(|e| e.into_inner());
                    }
                    state.waiting_producers = state.waiting_producers.saturating_sub(1);
                    if state.stopped {
                        return Ok(len);
                    }
                }
                Some(deadline) => {
                    state.waiting_producers += 1;
                    while !state.stopped && state.buffered_lines >= self.shared.limit {
                        let now = Instant::now();
                        if now >= deadline {
                            state.waiting_producers = state.waiting_producers.saturating_sub(1);
                            self.shared.dropped_lines.incr_saturating();
                            return Ok(len);
                        }
                        let (next_state, _) = self
                            .shared
                            .state_changed
                            .wait_timeout(state, deadline - now)
                            .unwrap_or_else(|e| e.into_inner());
                        state = next_state;
                    }
                    state.waiting_producers = state.waiting_producers.saturating_sub(1);
                    if state.stopped {
                        return Ok(len);
                    }
                }
            }
        }

        let was_empty = state.deque.is_empty();
        state.deque.push_back(QueueItem::Line(buf.to_vec()));
        state.buffered_lines += 1;
        if was_empty {
            self.shared.not_empty.notify_one();
        }
        Ok(len)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    #[inline]
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.write(buf).map(|_| ())
    }
}

impl<'a> MakeWriter<'a> for OwlAsyncWriter {
    type Writer = OwlAsyncWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// 单个输出通道的弱引用刷盘句柄（不延长后台线程生命周期）。
#[derive(Clone)]
pub(crate) struct OwlFlushHandle {
    shared: Weak<AsyncQueueShared>,
    #[cfg(test)]
    default_timeout: Option<Duration>,
}

impl OwlFlushHandle {
    #[cfg(test)]
    pub(crate) fn flush(&self) -> Result<(), OwlError> {
        self.flush_until(compute_deadline(self.default_timeout))
    }

    #[cfg(test)]
    pub(crate) fn flush_timeout(&self, timeout: Option<Duration>) -> Result<(), OwlError> {
        self.flush_until(compute_deadline(timeout))
    }

    /// 仅向队列投递 Flush 控制帧并返回期待确认的序号（使多通道可并行刷盘后再共享同一个 deadline 等待）。
    fn enqueue_flush(&self) -> Option<(Arc<AsyncQueueShared>, u64)> {
        let shared = self.shared.upgrade()?;
        let mut state = shared.mutex.lock().unwrap_or_else(|e| e.into_inner());
        if state.worker_finished || state.worker_thread_id == Some(thread::current().id()) {
            return None;
        }
        state.next_flush_seq += 1;
        let seq = state.next_flush_seq;
        let was_empty = state.deque.is_empty();
        state.deque.push_back(QueueItem::Flush(seq));
        drop(state);
        if was_empty {
            shared.not_empty.notify_one();
        }
        Some((shared, seq))
    }

    fn wait_flushed(
        shared: &AsyncQueueShared,
        seq: u64,
        deadline: Option<Instant>,
    ) -> Result<(), OwlError> {
        let mut state = shared.mutex.lock().unwrap_or_else(|e| e.into_inner());
        if state.worker_finished || state.flushed_seq >= seq {
            return Ok(());
        }
        state.waiting_flushers += 1;
        let result = match deadline {
            None => {
                while !state.worker_finished && state.flushed_seq < seq {
                    state = shared
                        .state_changed
                        .wait(state)
                        .unwrap_or_else(|e| e.into_inner());
                }
                Ok(())
            }
            Some(limit) => loop {
                if state.worker_finished || state.flushed_seq >= seq {
                    break Ok(());
                }
                let now = Instant::now();
                if now >= limit {
                    break Err(OwlError::FlushTimeout);
                }
                let (next_state, _) = shared
                    .state_changed
                    .wait_timeout(state, limit - now)
                    .unwrap_or_else(|e| e.into_inner());
                state = next_state;
            },
        };
        state.waiting_flushers = state.waiting_flushers.saturating_sub(1);
        result
    }

    #[cfg(test)]
    pub(crate) fn flush_until(&self, deadline: Option<Instant>) -> Result<(), OwlError> {
        let Some((shared, seq)) = self.enqueue_flush() else {
            return Ok(());
        };
        Self::wait_flushed(&shared, seq, deadline)
    }
}

/// 聚合所有活跃通道的全局刷盘句柄（并行触发所有通道刷盘并共享同一总超时截止点）。
#[derive(Clone, Default)]
pub(crate) struct GlobalFlushHandle {
    pub(crate) handles: Vec<OwlFlushHandle>,
    pub(crate) default_timeout: Option<Duration>,
}

impl GlobalFlushHandle {
    pub(crate) fn flush(&self) -> Result<(), OwlError> {
        let deadline = compute_deadline(self.default_timeout);
        // 1. 先向全部活跃通道并行投递 Flush 控制帧
        let pending: Vec<_> = self
            .handles
            .iter()
            .filter_map(OwlFlushHandle::enqueue_flush)
            .collect();

        // 2. 共享同一 deadline 等待全部通道落盘确认
        let mut first_err = None;
        for (shared, seq) in pending {
            if let Err(err) = OwlFlushHandle::wait_flushed(&shared, seq, deadline) {
                if first_err.is_none() {
                    first_err = Some(err);
                }
            }
        }
        if let Some(err) = first_err {
            Err(err)
        } else {
            Ok(())
        }
    }
}

/// 持有异步工作线程生命周期的守护对象，Drop 时排空队列并等待底层文件关闭。
pub(crate) struct OwlWorkerGuard {
    shared: Arc<AsyncQueueShared>,
    join_handle: Option<JoinHandle<()>>,
    shutdown_timeout: Option<Duration>,
}

impl OwlWorkerGuard {
    /// 非阻塞通知工作线程开始排空并退出（供 `OwlGuard::drop` 并行广播给所有通道）。
    pub(crate) fn signal_shutdown(&self) {
        let mut state = self.shared.mutex.lock().unwrap_or_else(|e| e.into_inner());
        if !state.stopped {
            state.stopped = true;
            state.deque.push_back(QueueItem::Shutdown);
            self.shared.not_empty.notify_one();
            if state.waiting_producers > 0 || state.waiting_flushers > 0 {
                self.shared.state_changed.notify_all();
            }
        }
    }

    /// 按共享截止时间等待工作线程排空退出并回收句柄（幂等可重入）。
    pub(crate) fn wait_until(&mut self, deadline: Option<Instant>) {
        if self.join_handle.is_none() {
            return;
        }

        let is_same_thread = {
            let state = self.shared.mutex.lock().unwrap_or_else(|e| e.into_inner());
            state.worker_thread_id == Some(thread::current().id())
        };
        if is_same_thread {
            return;
        }

        self.signal_shutdown();

        match deadline {
            None => {
                if let Some(handle) = self.join_handle.take() {
                    let _ = handle.join();
                }
            }
            Some(limit) => {
                let mut finished = false;
                {
                    let mut state = self.shared.mutex.lock().unwrap_or_else(|e| e.into_inner());
                    state.waiting_flushers += 1;
                    while !state.worker_finished {
                        let now = Instant::now();
                        if now >= limit {
                            break;
                        }
                        let (next_state, _) = self
                            .shared
                            .state_changed
                            .wait_timeout(state, limit - now)
                            .unwrap_or_else(|e| e.into_inner());
                        state = next_state;
                    }
                    state.waiting_flushers = state.waiting_flushers.saturating_sub(1);
                    if state.worker_finished {
                        finished = true;
                    }
                }
                if finished {
                    if let Some(handle) = self.join_handle.take() {
                        let _ = handle.join();
                    }
                } else {
                    // 超时后显式分离句柄避免再次阻塞
                    let _ = self.join_handle.take();
                }
            }
        }
    }
}

impl Drop for OwlWorkerGuard {
    fn drop(&mut self) {
        if self.join_handle.is_some() {
            let deadline = compute_deadline(self.shutdown_timeout);
            self.wait_until(deadline);
        }
    }
}

struct CompressionJob {
    staging_path: PathBuf,
    dest_gz_path: PathBuf,
    max_files_prune: Option<(PathBuf, String, usize)>,
}

struct CompressionState {
    queue: VecDeque<CompressionJob>,
    in_flight: usize,
    shutdown: bool,
}

/// 单线程有序后台 Gzip 压缩器。
struct CompressionWorker {
    shared: Arc<(Mutex<CompressionState>, Condvar, Condvar)>,
    join_handle: Option<JoinHandle<()>>,
}

impl CompressionWorker {
    fn new(dir_lock: Arc<Mutex<()>>, sink_errors: AtomicCounter) -> Self {
        let shared = Arc::new((
            Mutex::new(CompressionState {
                queue: VecDeque::new(),
                in_flight: 0,
                shutdown: false,
            }),
            Condvar::new(),
            Condvar::new(),
        ));

        let worker_shared = Arc::clone(&shared);
        let join_handle = thread::Builder::new()
            .name("owl-logger-gzip".to_string())
            .spawn(move || {
                let (mutex, work_cv, idle_cv) = &*worker_shared;
                loop {
                    let job = {
                        let mut state = mutex.lock().unwrap_or_else(|e| e.into_inner());
                        while state.queue.is_empty() && !state.shutdown {
                            state = work_cv.wait(state).unwrap_or_else(|e| e.into_inner());
                        }
                        match state.queue.pop_front() {
                            Some(job) => job,
                            None => break,
                        }
                    };

                    let compress_res =
                        compress_file_with_lock(&job.staging_path, &job.dest_gz_path, &dir_lock);
                    if compress_res.is_err() {
                        sink_errors.incr_saturating();
                    }

                    if let Some((log_dir, file_name, max_files)) = job.max_files_prune {
                        let _guard = dir_lock.lock().unwrap_or_else(|e| e.into_inner());
                        if prune_excess_historical_logs(&log_dir, &file_name, max_files).is_err() {
                            sink_errors.incr_saturating();
                        }
                    }

                    let mut state = mutex.lock().unwrap_or_else(|e| e.into_inner());
                    state.in_flight = state.in_flight.saturating_sub(1);
                    if state.in_flight == 0 {
                        idle_cv.notify_all();
                    }
                }
            })
            .expect("owl-logger: failed to spawn compression worker thread");

        Self {
            shared,
            join_handle: Some(join_handle),
        }
    }

    fn enqueue(&self, job: CompressionJob) {
        let (mutex, work_cv, _) = &*self.shared;
        let mut state = mutex.lock().unwrap_or_else(|e| e.into_inner());
        state.in_flight += 1;
        state.queue.push_back(job);
        work_cv.notify_one();
    }

    fn wait_idle(&self) {
        let (mutex, _, idle_cv) = &*self.shared;
        let mut state = mutex.lock().unwrap_or_else(|e| e.into_inner());
        while state.in_flight > 0 {
            state = idle_cv.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl Drop for CompressionWorker {
    fn drop(&mut self) {
        {
            let (mutex, work_cv, _) = &*self.shared;
            let mut state = mutex.lock().unwrap_or_else(|e| e.into_inner());
            state.shutdown = true;
            work_cv.notify_all();
        }
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(unix)]
fn file_identity(meta: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

/// 支持按时间（每天、每小时，支持本地时区与 UTC）或大小限制自动轮转、
/// 外部误删自动重建自愈，且活跃日志文件名不含时间戳的自定义文件写入器。
pub(crate) struct OwlRollingFileWriter {
    pub(crate) log_dir: PathBuf,
    pub(crate) file_name: String,
    pub(crate) rotation: RotationPolicy,
    pub(crate) max_size: u64,
    pub(crate) max_files: Option<usize>,
    pub(crate) retention_days: Option<usize>,
    pub(crate) use_utc: bool,
    pub(crate) current_file: Option<BufWriter<std::fs::File>>,
    pub(crate) current_size: u64,
    pub(crate) active_date: Option<chrono::NaiveDate>,
    pub(crate) active_hour: Option<(chrono::NaiveDate, u32)>,
    #[cfg(unix)]
    active_identity: Option<(u64, u64)>,
    last_path_check: Option<Instant>,
    next_unbounded_index: Option<u64>,
    dir_lock: Arc<Mutex<()>>,
    sink_errors: AtomicCounter,
    compressor: Option<CompressionWorker>,
}

impl OwlRollingFileWriter {
    #[cfg(test)]
    pub(crate) fn new(
        log_dir: impl Into<PathBuf>,
        file_name: impl Into<String>,
        rotation: RotationPolicy,
        max_files: Option<usize>,
        retention_days: Option<usize>,
    ) -> Self {
        Self::with_options(
            log_dir,
            file_name,
            rotation,
            max_files,
            retention_days,
            false,
            Arc::new(Mutex::new(())),
            AtomicCounter::new(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_options(
        log_dir: impl Into<PathBuf>,
        file_name: impl Into<String>,
        rotation: RotationPolicy,
        max_files: Option<usize>,
        retention_days: Option<usize>,
        use_utc: bool,
        dir_lock: Arc<Mutex<()>>,
        sink_errors: AtomicCounter,
    ) -> Self {
        let max_size = match &rotation {
            RotationPolicy::SizeMB(mb) => mb.saturating_mul(1024 * 1024),
            _ => 0,
        };
        Self {
            log_dir: log_dir.into(),
            file_name: file_name.into(),
            rotation,
            max_size,
            max_files: max_files.filter(|&n| n > 0),
            retention_days: retention_days.filter(|&d| d > 0),
            use_utc,
            current_file: None,
            current_size: 0,
            active_date: None,
            active_hour: None,
            #[cfg(unix)]
            active_identity: None,
            last_path_check: None,
            next_unbounded_index: None,
            dir_lock,
            sink_errors,
            compressor: None,
        }
    }

    fn current_date_and_hour(&self) -> (chrono::NaiveDate, u32) {
        if self.use_utc {
            let now = chrono::Utc::now();
            (now.date_naive(), now.hour())
        } else {
            let now = chrono::Local::now();
            (now.date_naive(), now.hour())
        }
    }

    fn system_time_date_and_hour(&self, time: SystemTime) -> (chrono::NaiveDate, u32) {
        if self.use_utc {
            let dt: chrono::DateTime<chrono::Utc> = time.into();
            (dt.date_naive(), dt.hour())
        } else {
            let dt: chrono::DateTime<chrono::Local> = time.into();
            (dt.date_naive(), dt.hour())
        }
    }

    /// 检测活跃日志文件是否被外部运维工具删除或移动替换（节流检查，缺失或 inode 变化时自动重开自愈）。
    pub(crate) fn reopen_if_missing_or_replaced(&mut self, force: bool) -> io::Result<()> {
        if self.current_file.is_none() {
            return Ok(());
        }

        let now = Instant::now();
        if !force {
            if let Some(last) = self.last_path_check {
                if now.saturating_duration_since(last) < PATH_CHECK_INTERVAL {
                    return Ok(());
                }
            }
        }
        self.last_path_check = Some(now);

        let file_path = self.log_dir.join(format!("{}.log", self.file_name));
        let needs_reopen = match std::fs::metadata(&file_path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => true,
            Err(_) => false,
            Ok(meta) => {
                #[cfg(unix)]
                {
                    self.active_identity
                        .is_some_and(|expected| file_identity(&meta) != expected)
                }
                #[cfg(not(unix))]
                {
                    let _ = meta;
                    false
                }
            }
        };

        if needs_reopen {
            if let Some(mut old) = self.current_file.take() {
                let _ = old.flush();
            }
            #[cfg(unix)]
            {
                self.active_identity = None;
            }
            std::fs::create_dir_all(&self.log_dir)?;
            let reopened = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file_path)?;
            let meta = reopened.metadata()?;
            self.current_size = meta.len();
            #[cfg(unix)]
            {
                self.active_identity = Some(file_identity(&meta));
            }
            self.current_file = Some(BufWriter::with_capacity(FILE_BUF_CAPACITY, reopened));
        }
        Ok(())
    }

    pub(crate) fn init_file(&mut self) -> io::Result<&mut BufWriter<std::fs::File>> {
        if self.current_file.is_none() {
            let file_path = self.log_dir.join(format!("{}.log", self.file_name));

            // 检查启动时是否需要轮转已经存在的老日志文件
            if file_path.exists() {
                let (now_date, now_hour) = self.current_date_and_hour();
                let mut rotate_needed = false;
                let mut rotation_date = None;
                let mut rotation_hour = None;

                if let Ok(metadata) = file_path.metadata() {
                    if let Ok(modified) = metadata.modified() {
                        let (mod_date, mod_hour_val) = self.system_time_date_and_hour(modified);
                        match &self.rotation {
                            RotationPolicy::Daily => {
                                if mod_date != now_date {
                                    rotate_needed = true;
                                    rotation_date = Some(mod_date);
                                }
                            }
                            RotationPolicy::Hourly => {
                                let mod_hour = (mod_date, mod_hour_val);
                                let cur_hour = (now_date, now_hour);
                                if mod_hour != cur_hour {
                                    rotate_needed = true;
                                    rotation_hour = Some(mod_hour);
                                }
                            }
                            RotationPolicy::SizeMB(_) => {
                                if metadata.len() > 0 && metadata.len() >= self.max_size {
                                    rotate_needed = true;
                                }
                            }
                            RotationPolicy::Never => {}
                        }
                    }
                }

                if rotate_needed {
                    let staging_path = unique_staging_path(&self.log_dir, &self.file_name);
                    if std::fs::rename(&file_path, &staging_path).is_ok() {
                        if let Err(err) = self.archive_staged_file(
                            &staging_path,
                            &file_path,
                            rotation_date,
                            rotation_hour,
                            true,
                        ) {
                            let _ = std::fs::rename(&staging_path, &file_path);
                            return Err(err);
                        }
                    }
                }
            }

            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file_path)?;

            let metadata = file.metadata()?;
            self.current_size = metadata.len();
            #[cfg(unix)]
            {
                self.active_identity = Some(file_identity(&metadata));
            }
            self.last_path_check = Some(Instant::now());

            let (now_date, now_hour) = self.current_date_and_hour();
            self.active_date = Some(now_date);
            self.active_hour = Some((now_date, now_hour));
            self.current_file = Some(BufWriter::with_capacity(FILE_BUF_CAPACITY, file));
            let _ = self.enforce_max_files();
        }

        self.current_file
            .as_mut()
            .ok_or_else(|| io::Error::other("owl-logger: rolling file was not initialized"))
    }

    fn next_monotonic_size_index(&mut self) -> u64 {
        if let Some(next) = self.next_unbounded_index {
            self.next_unbounded_index = Some(next.saturating_add(1));
            return next;
        }
        let max_existing = scan_max_size_index(&self.log_dir, &self.file_name);
        let chosen = max_existing.saturating_add(1);
        self.next_unbounded_index = Some(chosen.saturating_add(1));
        chosen
    }

    /// 归档已重命名为 `staging_path` 的旧日志文件。
    fn archive_staged_file(
        &mut self,
        staging_path: &Path,
        original_file_path: &Path,
        rotation_date: Option<chrono::NaiveDate>,
        rotation_hour: Option<(chrono::NaiveDate, u32)>,
        restore_on_error: bool,
    ) -> io::Result<()> {
        match &self.rotation {
            RotationPolicy::SizeMB(_) => {
                if let Some(compressor) = &self.compressor {
                    compressor.wait_idle();
                }

                let dir_lock = Arc::clone(&self.dir_lock);
                let dest_gz_opt = {
                    let _guard = dir_lock.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(max_files) = self.max_files {
                        if max_files > 1 {
                            let oldest_index = max_files - 1;
                            remove_file_if_exists(
                                &self
                                    .log_dir
                                    .join(format!("{}.{}.log.gz", self.file_name, oldest_index)),
                            )?;
                            remove_file_if_exists(
                                &self
                                    .log_dir
                                    .join(format!("{}.{}.log", self.file_name, oldest_index)),
                            )?;

                            for index in (1..oldest_index).rev() {
                                let source_gz = self
                                    .log_dir
                                    .join(format!("{}.{}.log.gz", self.file_name, index));
                                let destination_gz = self.log_dir.join(format!(
                                    "{}.{}.log.gz",
                                    self.file_name,
                                    index + 1
                                ));
                                rename_file_if_exists(&source_gz, &destination_gz)?;

                                let source_log = self
                                    .log_dir
                                    .join(format!("{}.{}.log", self.file_name, index));
                                let destination_log = self.log_dir.join(format!(
                                    "{}.{}.log",
                                    self.file_name,
                                    index + 1
                                ));
                                rename_file_if_exists(&source_log, &destination_log)?;
                            }

                            Some(self.log_dir.join(format!("{}.1.log.gz", self.file_name)))
                        } else {
                            remove_file_if_exists(staging_path)?;
                            None
                        }
                    } else {
                        let index = self.next_monotonic_size_index();
                        Some(
                            self.log_dir
                                .join(format!("{}.{}.log.gz", self.file_name, index)),
                        )
                    }
                };

                if let Some(dest_gz_path) = dest_gz_opt {
                    let compressor = self.compressor.get_or_insert_with(|| {
                        CompressionWorker::new(Arc::clone(&self.dir_lock), self.sink_errors.clone())
                    });
                    let max_files_prune = self
                        .max_files
                        .map(|max_files| (self.log_dir.clone(), self.file_name.clone(), max_files));
                    compressor.enqueue(CompressionJob {
                        staging_path: staging_path.to_path_buf(),
                        dest_gz_path,
                        max_files_prune,
                    });
                }
            }
            RotationPolicy::Daily | RotationPolicy::Hourly => {
                let _guard = self.dir_lock.lock().unwrap_or_else(|e| e.into_inner());
                let date_str = match self.rotation {
                    RotationPolicy::Daily => {
                        let date = rotation_date.unwrap_or_else(|| self.current_date_and_hour().0);
                        date.format("%Y-%m-%d").to_string()
                    }
                    RotationPolicy::Hourly => {
                        let (date, hour) =
                            rotation_hour.unwrap_or_else(|| self.current_date_and_hour());
                        format!("{}-{hour:02}", date.format("%Y-%m-%d"))
                    }
                    _ => unreachable!("only time-based rotation reaches this branch"),
                };

                let mut dest_path = self
                    .log_dir
                    .join(format!("{}.{}.log", self.file_name, date_str));
                if dest_path.exists() {
                    let mut index = 1usize;
                    loop {
                        let candidate = self
                            .log_dir
                            .join(format!("{}.{}.{}.log", self.file_name, date_str, index));
                        if !candidate.exists() {
                            dest_path = candidate;
                            break;
                        }
                        index += 1;
                    }
                }

                if let Err(error) = std::fs::rename(staging_path, &dest_path) {
                    if restore_on_error {
                        let _ = std::fs::rename(staging_path, original_file_path);
                    }
                    return Err(error);
                }
            }
            RotationPolicy::Never => {}
        }
        Ok(())
    }

    fn rotate(&mut self, now_date: chrono::NaiveDate, now_hour: u32) -> io::Result<()> {
        if let Some(mut buffered) = self.current_file.take() {
            let _ = buffered.flush();
        }
        #[cfg(unix)]
        {
            self.active_identity = None;
        }

        let file_path = self.log_dir.join(format!("{}.log", self.file_name));
        let rotation_date = self.active_date;
        let rotation_hour = self.active_hour;

        let staged = if file_path.exists() {
            let staging_path = unique_staging_path(&self.log_dir, &self.file_name);
            match std::fs::rename(&file_path, &staging_path) {
                Ok(()) => Some(staging_path),
                Err(err) if err.kind() == io::ErrorKind::NotFound => None,
                Err(err) => {
                    if let Ok(reopened) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&file_path)
                    {
                        if let Ok(meta) = reopened.metadata() {
                            self.current_size = meta.len();
                            #[cfg(unix)]
                            {
                                self.active_identity = Some(file_identity(&meta));
                            }
                        }
                        self.current_file =
                            Some(BufWriter::with_capacity(FILE_BUF_CAPACITY, reopened));
                    }
                    return Err(err);
                }
            }
        } else {
            None
        };

        let new_file = match std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&file_path)
        {
            Ok(file) => file,
            Err(err) => {
                if let Some(ref staging_path) = staged {
                    let _ = std::fs::rename(staging_path, &file_path);
                }
                return Err(err);
            }
        };

        #[cfg(unix)]
        if let Ok(meta) = new_file.metadata() {
            self.active_identity = Some(file_identity(&meta));
        }
        self.last_path_check = Some(Instant::now());
        self.current_size = 0;
        self.active_date = Some(now_date);
        self.active_hour = Some((now_date, now_hour));
        self.current_file = Some(BufWriter::with_capacity(FILE_BUF_CAPACITY, new_file));

        let mut post_rotate_error: Option<io::Error> = None;

        if let Some(staging_path) = staged {
            if let Err(err) = self.archive_staged_file(
                &staging_path,
                &file_path,
                rotation_date,
                rotation_hour,
                false,
            ) {
                post_rotate_error = Some(err);
            }
        }

        if let Some(retention_days) = self.retention_days {
            cleanup_old_logs_locked(
                &self.log_dir,
                &self.file_name,
                retention_days,
                &self.dir_lock,
            );
        }

        if !matches!(self.rotation, RotationPolicy::SizeMB(_)) {
            if let Err(err) = self.enforce_max_files() {
                if post_rotate_error.is_none() {
                    post_rotate_error = Some(err);
                }
            }
        }

        if let Some(err) = post_rotate_error {
            return Err(err);
        }
        Ok(())
    }

    fn enforce_max_files(&self) -> io::Result<()> {
        if let Some(max_files) = self.max_files {
            let _guard = self.dir_lock.lock().unwrap_or_else(|e| e.into_inner());
            prune_excess_historical_logs(&self.log_dir, &self.file_name, max_files)?;
        }
        Ok(())
    }
}

impl Write for OwlRollingFileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.init_file()?;
        self.reopen_if_missing_or_replaced(false)?;

        let mut rotate_needed = false;
        let mut rotation_time = None;

        match &self.rotation {
            RotationPolicy::Daily => {
                let (now_date, now_hour) = self.current_date_and_hour();
                rotation_time = Some((now_date, now_hour));
                if let Some(active_date) = self.active_date {
                    if now_date != active_date {
                        rotate_needed = true;
                    }
                }
            }
            RotationPolicy::Hourly => {
                let (now_date, now_hour) = self.current_date_and_hour();
                rotation_time = Some((now_date, now_hour));
                if let Some((active_date, active_hour)) = self.active_hour {
                    if now_date != active_date || now_hour != active_hour {
                        rotate_needed = true;
                    }
                }
            }
            RotationPolicy::SizeMB(_) => {
                if self.current_size > 0
                    && self.current_size.saturating_add(buf.len() as u64) > self.max_size
                {
                    rotate_needed = true;
                }
            }
            RotationPolicy::Never => {}
        }

        let mut post_rotate_error: Option<io::Error> = None;
        if rotate_needed {
            let (now_date, now_hour) =
                rotation_time.unwrap_or_else(|| self.current_date_and_hour());
            if let Err(err) = self.rotate(now_date, now_hour) {
                if self.current_file.is_none() {
                    return Err(err);
                }
                post_rotate_error = Some(err);
            }
        }

        let file = self
            .current_file
            .as_mut()
            .ok_or_else(|| io::Error::other("owl-logger: active log file unavailable"))?;
        let written = file.write(buf)?;
        self.current_size = self.current_size.saturating_add(written as u64);

        if let Some(err) = post_rotate_error {
            return Err(err);
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(ref mut file) = self.current_file {
            file.flush()?;
        }
        Ok(())
    }
}

impl Drop for OwlRollingFileWriter {
    fn drop(&mut self) {
        if let Some(ref mut file) = self.current_file {
            let _ = file.flush();
        }
        self.current_file = None;
    }
}

pub(crate) fn unique_staging_path(log_dir: &Path, file_name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let thread_id = format!("{:?}", thread::current().id());
    log_dir.join(format!(
        "{file_name}.rotate-staging.{nanos}.{thread_id}.log"
    ))
}

fn compress_file_with_lock(
    src_path: &Path,
    dest_path: &Path,
    dir_lock: &Mutex<()>,
) -> io::Result<()> {
    let tmp_path = unique_temp_gzip_path(dest_path);
    let write_res = (|| -> io::Result<()> {
        let src = std::fs::File::open(src_path)?;
        let dest = std::fs::File::create(&tmp_path)?;
        let mut encoder = flate2::write::GzEncoder::new(dest, flate2::Compression::default());
        let mut reader = io::BufReader::new(src);
        io::copy(&mut reader, &mut encoder)?;
        encoder.finish()?;
        Ok(())
    })();

    if let Err(err) = write_res {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }

    let finalize_res = {
        let _guard = dir_lock.lock().unwrap_or_else(|e| e.into_inner());
        remove_file_if_exists(dest_path).and_then(|()| std::fs::rename(&tmp_path, dest_path))
    };

    if finalize_res.is_ok() {
        let _ = remove_file_if_exists(src_path);
    } else {
        let _ = std::fs::remove_file(&tmp_path);
    }

    finalize_res
}

pub(crate) fn remove_file_if_exists(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn rename_file_if_exists(src: &Path, dst: &Path) -> io::Result<()> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn unique_temp_gzip_path(dest_path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let thread_id = format!("{:?}", thread::current().id());
    let file_name = dest_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("owl-log.gz");
    dest_path.with_file_name(format!("{file_name}.{nanos}.{thread_id}.tmp"))
}

fn extract_numeric_size_index(filename: &str, file_name: &str) -> Option<u64> {
    let dot_prefix = format!("{file_name}.");
    let remaining = filename.strip_prefix(&dot_prefix)?;
    let idx_str = remaining
        .strip_suffix(".log.gz")
        .or_else(|| remaining.strip_suffix(".log"))?;
    if idx_str.is_empty() || !idx_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    idx_str.parse::<u64>().ok()
}

/// 扫描当前目录下 `{file_name}.<idx>.log[.gz]` 的最大数字编号，避免过期清理删除低位文件后复用编号。
fn scan_max_size_index(log_dir: &Path, file_name: &str) -> u64 {
    let mut max_index = 0u64;
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(filename) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Some(idx) = extract_numeric_size_index(filename, file_name) {
            max_index = max_index.max(idx);
        }
    }
    max_index
}

fn is_daily_date_str(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

fn is_hourly_date_str(s: &str) -> bool {
    let Some((date, hour)) = s.rsplit_once('-') else {
        return false;
    };

    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok()
        && hour
            .parse::<u32>()
            .is_ok_and(|hour| (0..=23).contains(&hour))
}

fn matches_legacy_time_rotation_suffix(suffix: &str) -> bool {
    is_daily_date_str(suffix) || is_hourly_date_str(suffix)
}

fn matches_suffix(mut s: &str) -> bool {
    if s == ".log" || s == ".log.gz" {
        return true;
    }
    if s.starts_with('.') {
        s = &s[1..];
        if let Some(next_dot) = s.find('.') {
            let index_part = &s[..next_dot];
            if index_part.chars().all(|c| c.is_ascii_digit()) && !index_part.is_empty() {
                let rest = &s[next_dot..];
                if rest == ".log" || rest == ".log.gz" {
                    return true;
                }
            }
        }
    }
    false
}

/// 精确判定文件名是否是由特定前缀的日志组件生成的（包括其轮转产生的备份文件，无递归）
pub(crate) fn is_log_file_for_prefix_non_recursive(filename: &str, file_name: &str) -> bool {
    if filename == format!("{file_name}.log") || filename == format!("{file_name}.log.gz") {
        return true;
    }

    let log_dot = format!("{file_name}.log.");
    if filename
        .strip_prefix(&log_dot)
        .is_some_and(matches_legacy_time_rotation_suffix)
    {
        return true;
    }

    let dot_prefix = format!("{file_name}.");
    if filename.starts_with(&dot_prefix) {
        let remaining = &filename[dot_prefix.len()..];
        if let Some(first_dot) = remaining.find('.') {
            let part = &remaining[..first_dot];
            let suffix = &remaining[first_dot..];
            if matches_suffix(suffix) {
                if part.chars().all(|c| c.is_ascii_digit()) && !part.is_empty() {
                    return true;
                }
                if is_daily_date_str(part) {
                    return true;
                }
                if is_hourly_date_str(part) {
                    return true;
                }
            }
        }
    }

    false
}

pub(crate) fn is_log_file_for_prefix(filename: &str, file_name: &str) -> bool {
    if is_log_file_for_prefix_non_recursive(filename, file_name) {
        return true;
    }

    for level in &["error", "warn", "info", "debug", "trace"] {
        let level_prefix = format!("{file_name}.{level}");
        if is_log_file_for_prefix_non_recursive(filename, &level_prefix) {
            return true;
        }
    }

    false
}

fn is_orphaned_temp_for_prefix_non_recursive(filename: &str, file_name: &str) -> bool {
    let staging_prefix = format!("{file_name}.rotate-staging.");
    if let Some(rest) = filename.strip_prefix(&staging_prefix) {
        if rest.ends_with(".log") && rest.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
            return true;
        }
    }
    let dot_prefix = format!("{file_name}.");
    if let Some(rest) = filename.strip_prefix(&dot_prefix) {
        if let Some((idx_part, after_gz)) = rest.split_once(".log.gz.") {
            if !idx_part.is_empty()
                && idx_part.bytes().all(|b| b.is_ascii_digit())
                && after_gz.ends_with(".tmp")
            {
                return true;
            }
        }
    }
    false
}

fn is_orphaned_temp_for_prefix(filename: &str, file_name: &str) -> bool {
    if is_orphaned_temp_for_prefix_non_recursive(filename, file_name) {
        return true;
    }
    for level in &["error", "warn", "info", "debug", "trace"] {
        let level_prefix = format!("{file_name}.{level}");
        if is_orphaned_temp_for_prefix_non_recursive(filename, &level_prefix) {
            return true;
        }
    }
    false
}

/// 在不触及活跃日志的前提下，幂等删除超过数量上限的最早历史日志。
pub(crate) fn prune_excess_historical_logs(
    log_dir: &Path,
    file_name: &str,
    max_files: usize,
) -> io::Result<()> {
    let max_historical_files = max_files.saturating_sub(1);
    let active_file = format!("{file_name}.log");
    let mut historical_files = Vec::new();

    for entry in std::fs::read_dir(log_dir)? {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if filename == active_file || !is_log_file_for_prefix_non_recursive(filename, file_name) {
            continue;
        }

        let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
            continue;
        };
        let size_idx = extract_numeric_size_index(filename, file_name);
        historical_files.push((modified, size_idx, path));
    }

    historical_files.sort_by(
        |(left_time, left_idx, left_path), (right_time, right_idx, right_path)| {
            left_time
                .cmp(right_time)
                .then_with(|| match (left_idx, right_idx) {
                    // 在 `max_files = Some(N)` 的移位轮转模式下，数字编号越大表示历史越久远（如 `.2.log.gz` 早于 `.1.log.gz`），
                    // 当毫秒级连续轮转产生相同 mtime 时，优先淘汰编号更大的旧归档。
                    (Some(l), Some(r)) => r.cmp(l),
                    _ => left_path.cmp(right_path),
                })
        },
    );

    let excess = historical_files.len().saturating_sub(max_historical_files);
    for (_, _, path) in historical_files.into_iter().take(excess) {
        remove_file_if_exists(&path)?;
    }

    Ok(())
}

/// 持有目录维护锁执行过期日志清理。
pub(crate) fn cleanup_old_logs_locked(
    log_dir: &Path,
    file_name: &str,
    retention_days: usize,
    dir_lock: &Mutex<()>,
) {
    let _guard = dir_lock.lock().unwrap_or_else(|e| e.into_inner());
    cleanup_old_logs(log_dir, file_name, retention_days);
}

/// 清理过期日志文件（`retention_days == 0` 表示关闭按天清理）。
pub(crate) fn cleanup_old_logs(log_dir: &Path, file_name: &str, retention_days: usize) {
    if retention_days == 0 {
        return;
    }

    let now = SystemTime::now();
    let max_age = Duration::from_secs((retention_days as u64).saturating_mul(24 * 60 * 60));
    let orphan_age = max_age.max(ORPHAN_STAGING_MIN_AGE);

    let log_exact = format!("{file_name}.log");
    let active_error_log_prefixes = [
        format!("{file_name}.error.log"),
        format!("{file_name}.warn.log"),
        format!("{file_name}.info.log"),
        format!("{file_name}.debug.log"),
        format!("{file_name}.trace.log"),
    ];

    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(filename_str) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };

        if is_log_file_for_prefix(filename_str, file_name) {
            let is_active = filename_str == log_exact
                || active_error_log_prefixes.iter().any(|p| filename_str == p);
            if !is_active {
                if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
                    if let Ok(age) = now.duration_since(modified) {
                        if age > max_age {
                            let _ = remove_file_if_exists(&path);
                        }
                    }
                }
            }
        } else if is_orphaned_temp_for_prefix(filename_str, file_name) {
            if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
                if let Ok(age) = now.duration_since(modified) {
                    if age > orphan_age {
                        let _ = remove_file_if_exists(&path);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    struct BlockingFailWriter {
        block_gate: Arc<(Mutex<bool>, Condvar)>,
        first_write_barrier: Arc<Barrier>,
        fail_writes: bool,
    }

    impl Write for BlockingFailWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let (mutex, cv) = &*self.block_gate;
            let mut blocked = mutex.lock().unwrap();
            if *blocked {
                self.first_write_barrier.wait();
                while *blocked {
                    blocked = cv.wait(blocked).unwrap();
                }
            }
            if self.fail_writes {
                return Err(io::Error::other("simulated disk full"));
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.fail_writes {
                return Err(io::Error::other("simulated flush failure"));
            }
            Ok(())
        }
    }

    #[test]
    fn async_writer_tracks_bounded_timeout_drops_and_sink_errors() {
        let gate = Arc::new((Mutex::new(true), Condvar::new()));
        let barrier = Arc::new(Barrier::new(2));
        let mock = BlockingFailWriter {
            block_gate: Arc::clone(&gate),
            first_write_barrier: Arc::clone(&barrier),
            fail_writes: true,
        };

        let (mut writer, guard, flush_handle) = OwlAsyncWriter::spawn(
            mock,
            OwlAsyncWriterOptions {
                buffered_lines_limit: 2,
                lossy: false,
                queue_timeout: Some(Duration::from_millis(15)),
                shutdown_timeout: Some(Duration::from_secs(5)),
                thread_name: "test-async-writer".to_string(),
                sink_errors: None,
            },
        );
        let dropped = writer.dropped_counter();
        let sink_errors = writer.sink_error_counter();

        writer.write_all(b"line-1\n").unwrap();
        barrier.wait();

        writer.write_all(b"line-2\n").unwrap();
        writer.write_all(b"line-3\n").unwrap();
        assert_eq!(dropped.get(), 0);

        writer.write_all(b"line-4\n").unwrap();
        assert_eq!(dropped.get(), 1);

        assert!(matches!(
            flush_handle.flush_timeout(Some(Duration::from_millis(10))),
            Err(OwlError::FlushTimeout)
        ));

        {
            let (mutex, cv) = &*gate;
            *mutex.lock().unwrap() = false;
            cv.notify_all();
        }

        flush_handle.flush().unwrap();
        assert!(
            sink_errors.get() >= 3,
            "expected sink errors to be recorded, got {}",
            sink_errors.get()
        );

        drop(guard);
    }

    #[test]
    fn extreme_duration_max_does_not_overflow_instant_addition() {
        assert_eq!(compute_deadline(Some(Duration::MAX)), None);

        let (mut writer, guard, flush_handle) = OwlAsyncWriter::spawn(
            Vec::<u8>::new(),
            OwlAsyncWriterOptions {
                buffered_lines_limit: 8,
                lossy: false,
                queue_timeout: Some(Duration::MAX),
                shutdown_timeout: Some(Duration::MAX),
                thread_name: "test-max-duration".to_string(),
                sink_errors: None,
            },
        );
        writer.write_all(b"safe\n").unwrap();
        flush_handle.flush().unwrap();
        drop(guard);
    }

    #[test]
    fn rolling_writer_self_heals_when_active_file_is_deleted_externally() {
        let temp_dir = std::env::temp_dir().join(format!(
            "owl-test-self-heal-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let mut writer =
            OwlRollingFileWriter::new(&temp_dir, "heal", RotationPolicy::Never, None, None);
        writer.write_all(b"before unlink\n").unwrap();
        writer.flush().unwrap();

        let active_path = temp_dir.join("heal.log");
        assert!(active_path.exists());

        // 模拟外部运维误删活跃日志文件
        std::fs::remove_file(&active_path).unwrap();
        assert!(!active_path.exists());

        // 清除节流时间戳，模拟到达下一次路径探测窗口
        writer.last_path_check = None;
        writer.write_all(b"after unlink\n").unwrap();
        writer.flush().unwrap();
        drop(writer);

        assert!(active_path.exists(), "deleted active log must be recreated");
        assert_eq!(
            std::fs::read_to_string(&active_path).unwrap(),
            "after unlink\n"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn same_mtime_size_archives_prune_higher_index_first() {
        let temp_dir = std::env::temp_dir().join(format!(
            "owl-test-tie-prune-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let active = temp_dir.join("app.log");
        let newer_archive = temp_dir.join("app.1.log.gz");
        let older_archive = temp_dir.join("app.2.log.gz");
        let same_time = UNIX_EPOCH + Duration::from_secs(1_700_000_000);

        for file in [&active, &newer_archive, &older_archive] {
            std::fs::write(file, b"data").unwrap();
            std::fs::OpenOptions::new()
                .write(true)
                .open(file)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(same_time))
                .unwrap();
        }

        // max_files = 2 意味着保留 app.log + 1 个最新历史归档（app.1.log.gz）
        prune_excess_historical_logs(&temp_dir, "app", 2).unwrap();
        assert!(
            newer_archive.exists(),
            "newer archive app.1.log.gz must be kept"
        );
        assert!(
            !older_archive.exists(),
            "older archive app.2.log.gz must be pruned first on mtime tie"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
