use crate::config::Language;
use crate::i18n::I18n;

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
pub struct OwlGuard {
    /// 文件写入器的 WorkerGuard
    pub(crate) _file_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
    /// 控制台写入器的 WorkerGuard
    pub(crate) _console_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
    /// 当前语言设置（用于 Drop 时的提示信息）
    pub(crate) language: Language,
}

impl Drop for OwlGuard {
    fn drop(&mut self) {
        // 打印清理提示（直接输出到 stderr 避免通过已关闭的 subscriber）
        eprintln!("{}", I18n::cleanup_message(self.language));
        // WorkerGuard 的 Drop 实现会自动 flush 所有缓冲日志
    }
}
