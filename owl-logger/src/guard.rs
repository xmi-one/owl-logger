use std::time::Duration;

use crate::config::Language;
use crate::error::OwlError;
use crate::i18n::I18n;
use crate::writer::{compute_deadline, AtomicCounter, GlobalFlushHandle, OwlWorkerGuard};

/// 各输出通道因队列已满或阻塞超时丢弃的日志行数。
///
/// 同一事件同时写入多个通道时会分别计数，因此 total 是输出行总数，
/// 而不是唯一事件数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DroppedLogLines {
    pub console: usize,
    pub file: usize,
    pub error_file: usize,
}

impl DroppedLogLines {
    /// 所有输出通道已丢弃的日志行总数。
    pub fn total(self) -> usize {
        self.console
            .saturating_add(self.file)
            .saturating_add(self.error_file)
    }
}

/// 各输出通道底层 I/O（写入、刷盘、轮转或后台压缩）发生的错误次数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SinkErrors {
    pub console: usize,
    pub file: usize,
    pub error_file: usize,
}

impl SinkErrors {
    /// 所有输出通道发生的底层 I/O 错误总数。
    pub fn total(self) -> usize {
        self.console
            .saturating_add(self.file)
            .saturating_add(self.error_file)
    }
}

/// 日志看门狗（Guard）
///
/// 持有非阻塞写入器的工作线程守护对象。当 `OwlGuard` 被丢弃（Drop）时，
/// 所有缓冲的日志条目会被并行排空刷盘，并等待轮转压缩完成，确保日志不会丢失。
///
/// # 用法
///
/// 在 `main()` 函数中用 `let _guard = ...` 持有该对象即可：
///
/// ```rust,no_run
/// let _guard = owl_logger::init();
/// tracing::info!("日志不会丢失");
/// // _guard 在此处被 Drop，自动 flush
/// ```
#[must_use = "keep OwlGuard alive until application shutdown so buffered logs can flush"]
pub struct OwlGuard {
    /// 周期清理任务。放在写入器 Guard 前面，以便关闭时先停止目录扫描。
    pub(crate) _cleanup_worker: Option<crate::builder::CleanupWorker>,
    /// 文件写入器的 WorkerGuard
    pub(crate) _file_guard: Option<OwlWorkerGuard>,
    /// 控制台写入器的 WorkerGuard
    pub(crate) _console_guard: Option<OwlWorkerGuard>,
    /// 分级独立文件写入器的 WorkerGuard（如 error.log）
    pub(crate) _error_file_guard: Option<OwlWorkerGuard>,
    /// 主文件通道的丢失行计数器
    pub(crate) _file_dropped_lines: Option<AtomicCounter>,
    /// 控制台通道的丢失行计数器
    pub(crate) _console_dropped_lines: Option<AtomicCounter>,
    /// 分级文件通道的丢失行计数器
    pub(crate) _error_file_dropped_lines: Option<AtomicCounter>,
    /// 主文件通道的底层 I/O 错误计数器
    pub(crate) _file_sink_errors: Option<AtomicCounter>,
    /// 控制台通道的底层 I/O 错误计数器
    pub(crate) _console_sink_errors: Option<AtomicCounter>,
    /// 分级文件通道的底层 I/O 错误计数器
    pub(crate) _error_file_sink_errors: Option<AtomicCounter>,
    /// 运行期主动排空队列的刷盘句柄
    pub(crate) _flush_handle: GlobalFlushHandle,
    /// 停机排空共享总超时
    pub(crate) shutdown_timeout: Option<Duration>,
    /// OTLP 追踪 provider（持有以便在 Drop 时 flush 并关闭导出）
    #[cfg(feature = "otlp")]
    pub(crate) _otel_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
    /// 当前语言设置（用于 Drop 时的提示信息）
    pub(crate) language: Language,
}

impl OwlGuard {
    /// 获取各输出通道因有损队列或阻塞超时已丢弃的日志行数。
    ///
    /// 当 builder 使用 `lossy(false)` 且未设置 `queue_timeout` 时，所有计数均应为 0。
    pub fn dropped_lines(&self) -> DroppedLogLines {
        DroppedLogLines {
            console: self
                ._console_dropped_lines
                .as_ref()
                .map_or(0, AtomicCounter::get),
            file: self
                ._file_dropped_lines
                .as_ref()
                .map_or(0, AtomicCounter::get),
            error_file: self
                ._error_file_dropped_lines
                .as_ref()
                .map_or(0, AtomicCounter::get),
        }
    }

    /// 获取各输出通道发生底层写入、轮转或压缩 I/O 错误的累计次数。
    pub fn sink_errors(&self) -> SinkErrors {
        SinkErrors {
            console: self
                ._console_sink_errors
                .as_ref()
                .map_or(0, AtomicCounter::get),
            file: self
                ._file_sink_errors
                .as_ref()
                .map_or(0, AtomicCounter::get),
            error_file: self
                ._error_file_sink_errors
                .as_ref()
                .map_or(0, AtomicCounter::get),
        }
    }

    /// 主动排空当前所有异步输出通道的缓冲区并刷盘（不关闭日志系统）。
    pub fn flush(&self) -> Result<(), OwlError> {
        self._flush_handle.flush()
    }
}

impl Drop for OwlGuard {
    fn drop(&mut self) {
        // 1. 打印清理提示
        tracing::info!("{}", I18n::cleanup_message(self.language));

        // 2. 先停止后台周期目录扫描线程
        self._cleanup_worker.take();

        // 3. 计算单一共享停机截止时间，先并行广播 Shutdown 给所有活跃通道，再共享剩余时间汇合，
        //    避免串行停机导致等待耗时放大为 3 × shutdown_timeout。
        let deadline = compute_deadline(self.shutdown_timeout);
        if let Some(ref guard) = self._file_guard {
            guard.signal_shutdown();
        }
        if let Some(ref guard) = self._console_guard {
            guard.signal_shutdown();
        }
        if let Some(ref guard) = self._error_file_guard {
            guard.signal_shutdown();
        }

        if let Some(ref mut guard) = self._file_guard {
            guard.wait_until(deadline);
        }
        if let Some(ref mut guard) = self._console_guard {
            guard.wait_until(deadline);
        }
        if let Some(ref mut guard) = self._error_file_guard {
            guard.wait_until(deadline);
        }

        // 4. 关闭 OTLP provider，确保缓冲的 span 在退出前被导出
        #[cfg(feature = "otlp")]
        if let Some(provider) = &self._otel_provider {
            let _ = provider.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DroppedLogLines, SinkErrors};

    #[test]
    fn dropped_log_lines_total_is_saturating() {
        let dropped = DroppedLogLines {
            console: usize::MAX,
            file: 1,
            error_file: 1,
        };

        assert_eq!(dropped.total(), usize::MAX);
    }

    #[test]
    fn sink_errors_total_is_saturating() {
        let errors = SinkErrors {
            console: usize::MAX,
            file: 2,
            error_file: 3,
        };

        assert_eq!(errors.total(), usize::MAX);
    }
}
