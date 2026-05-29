use std::fmt;

use owo_colors::OwoColorize;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

use crate::config::Language;
use crate::i18n::I18n;

/// owl-logger 自定义格式化器
///
/// 支持多语言日志级别名称和自定义输出格式。
/// 输出格式示例（中文）：
///
/// ```text
/// 2025-05-30 10:30:15 | 信息 | my_app::handler > 订单创建成功
/// ```
pub struct OwlFormatter {
    pub(crate) language: Language,
    pub(crate) show_target: bool,
    pub(crate) show_thread: bool,
    pub(crate) show_line_number: bool,
    pub(crate) enable_ansi: bool,
}

impl OwlFormatter {
    /// 格式化日志级别（带颜色和语言支持）
    fn format_level(&self, level: &Level) -> String {
        let name = I18n::level_name(level, self.language);
        let padded = format!("{:<5}", name);

        if self.enable_ansi {
            match *level {
                Level::TRACE => padded.purple().to_string(),
                Level::DEBUG => padded.blue().to_string(),
                Level::INFO => padded.green().to_string(),
                Level::WARN => padded.yellow().to_string(),
                Level::ERROR => padded.red().bold().to_string(),
            }
        } else {
            padded
        }
    }

    /// 格式化时间戳
    fn format_timestamp(&self) -> String {
        let now = chrono::Local::now();
        let ts = now.format("%Y-%m-%d %H:%M:%S").to_string();
        if self.enable_ansi {
            ts.dimmed().to_string()
        } else {
            ts
        }
    }

    /// 格式化分隔符
    fn format_separator(&self) -> String {
        if self.enable_ansi {
            " | ".dimmed().to_string()
        } else {
            " | ".to_string()
        }
    }
}

impl<S, N> FormatEvent<S, N> for OwlFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let sep = self.format_separator();
        let timestamp = self.format_timestamp();
        let level = self.format_level(event.metadata().level());

        // 时间 | 级别
        write!(writer, "{timestamp}{sep}{level}{sep}")?;

        // Span 上下文信息（如 request req_id="req-001"）
        if let Some(scope) = ctx.event_scope() {
            let mut first = true;
            for span in scope.from_root() {
                if !first {
                    write!(writer, ":")?;
                }
                let meta = span.metadata();
                if self.enable_ansi {
                    write!(writer, "{}", meta.name().cyan())?;
                } else {
                    write!(writer, "{}", meta.name())?;
                }

                let ext = span.extensions();
                if let Some(fields) = ext.get::<tracing_subscriber::fmt::FormattedFields<N>>() {
                    if !fields.is_empty() {
                        write!(writer, "{{{fields}}}")?;
                    }
                }
                first = false;
            }
            if !first {
                write!(writer, "{sep}")?;
            }
        }

        // 来源模块
        if self.show_target {
            let target = event.metadata().target();
            if self.enable_ansi {
                write!(writer, "{}{sep}", target.dimmed())?;
            } else {
                write!(writer, "{target}{sep}")?;
            }
        }

        // 线程信息
        if self.show_thread {
            let thread = std::thread::current();
            let thread_name = thread.name().unwrap_or("unnamed");
            if self.enable_ansi {
                write!(writer, "{}{sep}", thread_name.dimmed())?;
            } else {
                write!(writer, "{thread_name}{sep}")?;
            }
        }

        // 行号
        if self.show_line_number {
            if let Some(line) = event.metadata().line() {
                if let Some(file) = event.metadata().file() {
                    if self.enable_ansi {
                        write!(writer, "{}{sep}", format!("{file}:{line}").dimmed())?;
                    } else {
                        write!(writer, "{file}:{line}{sep}")?;
                    }
                }
            }
        }

        // 日志消息和结构化字段
        let mut fields_buf = String::new();
        ctx.format_fields(
            tracing_subscriber::fmt::format::Writer::new(&mut fields_buf),
            event,
        )?;

        if self.enable_ansi {
            let level = event.metadata().level();
            let colored_fields = match *level {
                Level::ERROR => fields_buf.red().bold().to_string(),
                Level::WARN => fields_buf.yellow().to_string(),
                Level::INFO => fields_buf,
                Level::DEBUG => fields_buf.dimmed().to_string(),
                Level::TRACE => fields_buf.dimmed().to_string(),
            };
            write!(writer, "{}", colored_fields)?;
        } else {
            write!(writer, "{}", fields_buf)?;
        }
        writeln!(writer)
    }
}

/// 用于文件输出的无色格式化器（禁用 ANSI 转义序列）
pub(crate) fn file_formatter(language: Language, config: &crate::config::OwlConfig) -> OwlFormatter {
    OwlFormatter {
        language,
        show_target: config.show_target,
        show_thread: config.show_thread,
        show_line_number: config.show_line_number,
        enable_ansi: false, // 文件输出不带颜色
    }
}

/// 用于控制台输出的彩色格式化器
pub(crate) fn console_formatter(language: Language, config: &crate::config::OwlConfig) -> OwlFormatter {
    OwlFormatter {
        language,
        show_target: config.show_target,
        show_thread: config.show_thread,
        show_line_number: config.show_line_number,
        enable_ansi: config.enable_ansi,
    }
}
