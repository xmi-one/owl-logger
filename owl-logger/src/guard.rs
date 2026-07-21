use crate::config::Language;
use crate::i18n::I18n;

/// 各输出通道因有损队列已丢弃的日志行数。
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

/// 日志看门狗（Guard）
///
/// 持有非阻塞写入器的 WorkerGuard。当 `OwlGuard` 被丢弃（Drop）时，
/// 所有缓冲的日志条目会被自动 flush 到输出端，确保日志不会丢失。
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
    pub(crate) _file_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
    /// 控制台写入器的 WorkerGuard
    pub(crate) _console_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
    /// 分级独立文件写入器的 WorkerGuard（如 error.log）
    pub(crate) _error_file_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
    /// 主文件通道的丢失行计数器
    pub(crate) _file_dropped_lines: Option<tracing_appender::non_blocking::ErrorCounter>,
    /// 控制台通道的丢失行计数器
    pub(crate) _console_dropped_lines: Option<tracing_appender::non_blocking::ErrorCounter>,
    /// 分级文件通道的丢失行计数器
    pub(crate) _error_file_dropped_lines: Option<tracing_appender::non_blocking::ErrorCounter>,
    /// OTLP 追踪 provider（持有以便在 Drop 时 flush 并关闭导出）
    #[cfg(feature = "otlp")]
    pub(crate) _otel_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
    /// 当前语言设置（用于 Drop 时的提示信息）
    pub(crate) language: Language,
}

impl OwlGuard {
    /// 获取各输出通道因有损队列已丢弃的日志行数。
    ///
    /// 当 builder 使用 lossy(false) 时，所有计数均应为 0。
    pub fn dropped_lines(&self) -> DroppedLogLines {
        DroppedLogLines {
            console: self._console_dropped_lines.as_ref().map_or(
                0,
                tracing_appender::non_blocking::ErrorCounter::dropped_lines,
            ),
            file: self._file_dropped_lines.as_ref().map_or(
                0,
                tracing_appender::non_blocking::ErrorCounter::dropped_lines,
            ),
            error_file: self._error_file_dropped_lines.as_ref().map_or(
                0,
                tracing_appender::non_blocking::ErrorCounter::dropped_lines,
            ),
        }
    }
}

impl Drop for OwlGuard {
    fn drop(&mut self) {
        // 打印清理提示
        tracing::info!("{}", I18n::cleanup_message(self.language));

        // 关闭 OTLP provider，确保缓冲的 span 在退出前被导出
        #[cfg(feature = "otlp")]
        if let Some(provider) = &self._otel_provider {
            let _ = provider.shutdown();
        }

        // WorkerGuard 的 Drop 实现会自动 flush 所有缓冲日志
    }
}

#[cfg(test)]
mod tests {
    use super::DroppedLogLines;

    #[test]
    fn dropped_log_lines_total_is_saturating() {
        let dropped = DroppedLogLines {
            console: usize::MAX,
            file: 1,
            error_file: 1,
        };

        assert_eq!(dropped.total(), usize::MAX);
    }
}
