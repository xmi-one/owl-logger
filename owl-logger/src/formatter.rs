use std::collections::HashMap;
use std::fmt;

use owo_colors::OwoColorize;
use tracing::span::{Attributes, Record};
use tracing::{Event, Id, Level, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

use crate::config::Language;
use crate::i18n::I18n;

const RESERVED_KEYS: &[&str] = &[
    "timestamp",
    "level",
    "message",
    "target",
    "thread",
    "file",
    "line",
];

/// 去除 `{:?}` 调试格式化产生的最外层引号（如 `"req-001"` -> `req-001`）
fn strip_debug_quotes(s: String) -> String {
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len() - 1].to_string()
    } else {
        s
    }
}

/// Escapes control characters, quotes, and backslashes before including user data in a
/// single-line text log. JSON output is serialized separately and does not use this helper.
fn escape_log_text(value: &str) -> std::str::EscapeDebug<'_> {
    value.escape_debug()
}

fn format_global_fields(global_fields: &HashMap<String, String>) -> String {
    use std::fmt::Write as _;

    let mut fields: Vec<_> = global_fields.iter().collect();
    fields.sort_unstable_by_key(|(key, _)| *key);

    let mut result = String::new();
    for (key, value) in fields {
        if !result.is_empty() {
            result.push(' ');
        }
        let _ = write!(result, "{}=\"{}\"", key, escape_log_text(value));
    }
    result
}

/// 结构化存储的 span 字段（保留插入顺序）。
///
/// 由 [`OwlSpanLayer`] 在 span 创建/记录时填充，格式化阶段直接读取，
/// 取代对格式化字符串做脆弱反向解析的旧实现。
#[derive(Debug, Default)]
pub(crate) struct OwlSpanFields(pub(crate) Vec<(String, serde_json::Value)>);

/// 把 span 字段收集为结构化 `serde_json::Value` 的访问器
struct SpanFieldVisitor<'a> {
    fields: &'a mut Vec<(String, serde_json::Value)>,
}

impl<'a> SpanFieldVisitor<'a> {
    fn push(&mut self, name: &str, value: serde_json::Value) {
        if let Some(slot) = self.fields.iter_mut().find(|(k, _)| k == name) {
            slot.1 = value;
        } else {
            self.fields.push((name.to_string(), value));
        }
    }
}

impl<'a> tracing::field::Visit for SpanFieldVisitor<'a> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.push(field.name(), serde_json::Value::String(value.to_string()));
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.push(
            field.name(),
            serde_json::Value::Number(serde_json::Number::from(value)),
        );
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.push(
            field.name(),
            serde_json::Value::Number(serde_json::Number::from(value)),
        );
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.push(field.name(), serde_json::Value::Bool(value));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        let val = serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .unwrap_or_else(|| serde_json::Value::String(value.to_string()));
        self.push(field.name(), val);
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let cleaned = strip_debug_quotes(format!("{:?}", value));
        self.push(field.name(), serde_json::Value::String(cleaned));
    }
}

/// 收集 span 字段并以结构化形式存入 span extensions 的 Layer
pub(crate) struct OwlSpanLayer;

impl<S> Layer<S> for OwlSpanLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let mut fields = Vec::new();
            attrs.record(&mut SpanFieldVisitor {
                fields: &mut fields,
            });
            span.extensions_mut().insert(OwlSpanFields(fields));
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let mut ext = span.extensions_mut();
            if let Some(existing) = ext.get_mut::<OwlSpanFields>() {
                values.record(&mut SpanFieldVisitor {
                    fields: &mut existing.0,
                });
            } else {
                let mut fields = Vec::new();
                values.record(&mut SpanFieldVisitor {
                    fields: &mut fields,
                });
                ext.insert(OwlSpanFields(fields));
            }
        }
    }
}

/// 插入用户提供的字段，同时保留已存在字段。
///
/// 事件字段优先以原名输出；保留键或同名字段会逐次添加下划线，而不是静默覆盖。
fn insert_json_field(
    log_obj: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: serde_json::Value,
) {
    let mut output_key = if RESERVED_KEYS.contains(&key) {
        format!("_{key}")
    } else {
        key.to_string()
    };

    while log_obj.contains_key(&output_key) {
        output_key.insert(0, '_');
    }

    log_obj.insert(output_key, value);
}

/// owl-logger 自定义格式化器（Pretty / 文本格式）
pub struct OwlFormatter {
    pub(crate) language: Language,
    pub(crate) compact: bool,
    pub(crate) show_target: bool,
    pub(crate) show_thread: bool,
    pub(crate) show_line_number: bool,
    pub(crate) enable_ansi: bool,
    pub(crate) time_format: String,
    pub(crate) use_utc: bool,
    pub(crate) global_fields: String,
}

impl OwlFormatter {
    /// 直接把级别名（带颜色）写入 writer，避免中间 String 分配。
    /// I18n::level_name 返回的名称已对齐到 5 字符宽度。
    fn write_level(&self, w: &mut Writer<'_>, level: &Level) -> fmt::Result {
        let name = I18n::level_name(level, self.language);
        if self.enable_ansi {
            match *level {
                Level::TRACE => write!(w, "{}", name.purple()),
                Level::DEBUG => write!(w, "{}", name.blue()),
                Level::INFO => write!(w, "{}", name.green()),
                Level::WARN => write!(w, "{}", name.yellow()),
                Level::ERROR => write!(w, "{}", name.red().bold()),
            }
        } else {
            write!(w, "{}", name)
        }
    }

    /// 直接把时间戳（带颜色）写入 writer，避免中间 String 分配
    fn write_timestamp(&self, w: &mut Writer<'_>) -> fmt::Result {
        let ts = if self.use_utc {
            chrono::Utc::now().format(&self.time_format)
        } else {
            chrono::Local::now().format(&self.time_format)
        };
        if self.enable_ansi {
            write!(w, "{}", ts.dimmed())
        } else {
            write!(w, "{}", ts)
        }
    }

    /// 直接把分隔符（带颜色）写入 writer，避免中间 String 分配
    fn write_separator(&self, w: &mut Writer<'_>) -> fmt::Result {
        if self.enable_ansi {
            write!(w, "{}", " | ".dimmed())
        } else {
            write!(w, " | ")
        }
    }
}

/// 用于 Pretty 格式的字段访问器
struct PrettyVisitor<'a> {
    message: &'a mut String,
    fields: &'a mut String,
}

impl<'a> tracing::field::Visit for PrettyVisitor<'a> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        use std::fmt::Write as _;
        let name = field.name();
        if name == "message" {
            self.message.clear();
            let _ = write!(self.message, "{}", escape_log_text(value));
            return;
        }
        if !self.fields.is_empty() {
            self.fields.push(' ');
        }
        let _ = write!(self.fields, "{}=\"{}\"", name, escape_log_text(value));
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        let name = field.name();
        if name == "message" {
            let val_str = format!("{:?}", value);
            *self.message = escape_log_text(&strip_debug_quotes(val_str)).to_string();
            return;
        }
        if !self.fields.is_empty() {
            self.fields.push(' ');
        }
        let value = format!("{:?}", value);
        let _ = write!(self.fields, "{}={}", name, escape_log_text(&value));
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
        use std::fmt::Write as _;

        // 时间 | 级别 |（Compact 格式使用空格分隔）
        self.write_timestamp(&mut writer)?;
        if self.compact {
            write!(writer, " ")?;
        } else {
            self.write_separator(&mut writer)?;
        }
        self.write_level(&mut writer, event.metadata().level())?;
        if self.compact {
            write!(writer, " ")?;
        } else {
            self.write_separator(&mut writer)?;
        }

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
                if let Some(fields) = ext.get::<OwlSpanFields>() {
                    if !fields.0.is_empty() {
                        let mut span_fields = String::new();
                        for (k, v) in &fields.0 {
                            if !span_fields.is_empty() {
                                span_fields.push_str(", ");
                            }
                            match v {
                                serde_json::Value::String(value) => {
                                    let _ =
                                        write!(span_fields, "{}=\"{}\"", k, escape_log_text(value));
                                }
                                value => {
                                    let value = value.to_string();
                                    let _ = write!(
                                        span_fields,
                                        "{}=\"{}\"",
                                        k,
                                        escape_log_text(&value)
                                    );
                                }
                            }
                        }

                        if !span_fields.is_empty() {
                            write!(writer, "{{{}}}", span_fields)?;
                        }
                    }
                }
                first = false;
            }
            if !first {
                if self.compact {
                    write!(writer, " ")?;
                } else {
                    self.write_separator(&mut writer)?;
                }
            }
        }

        // 来源模块
        if self.show_target {
            let target = event.metadata().target();
            if self.enable_ansi {
                write!(writer, "{}", target.dimmed())?;
            } else {
                write!(writer, "{target}")?;
            }
            if self.compact {
                write!(writer, ": ")?;
            } else {
                self.write_separator(&mut writer)?;
            }
        }

        // 线程信息
        if self.show_thread {
            let thread = std::thread::current();
            let thread_name = thread.name().unwrap_or("unnamed");
            if self.enable_ansi {
                write!(writer, "{}", thread_name.dimmed())?;
            } else {
                write!(writer, "{thread_name}")?;
            }
            if self.compact {
                write!(writer, " ")?;
            } else {
                self.write_separator(&mut writer)?;
            }
        }

        // 行号
        if self.show_line_number {
            if let Some(line) = event.metadata().line() {
                if let Some(file) = event.metadata().file() {
                    if self.enable_ansi {
                        write!(writer, "{}", format_args!("{file}:{line}").dimmed())?;
                    } else {
                        write!(writer, "{file}:{line}")?;
                    }
                    if self.compact {
                        write!(writer, " ")?;
                    } else {
                        self.write_separator(&mut writer)?;
                    }
                }
            }
        }

        // 提取日志消息与自定义字段
        let mut message = String::new();
        let mut fields_str = String::new();
        let mut visitor = PrettyVisitor {
            message: &mut message,
            fields: &mut fields_str,
        };
        event.record(&mut visitor);

        if self.enable_ansi {
            let full_msg = if self.global_fields.is_empty() {
                if fields_str.is_empty() {
                    message
                } else {
                    format!("{} {}", message, fields_str)
                }
            } else if fields_str.is_empty() {
                format!("{} {}", message, self.global_fields)
            } else {
                format!("{} {} {}", message, fields_str, self.global_fields)
            };

            let level = event.metadata().level();
            match *level {
                Level::ERROR => write!(writer, "{}", full_msg.red().bold())?,
                Level::WARN => write!(writer, "{}", full_msg.yellow())?,
                Level::INFO => write!(writer, "{}", full_msg)?,
                Level::DEBUG | Level::TRACE => write!(writer, "{}", full_msg.dimmed())?,
            }
        } else if fields_str.is_empty() && self.global_fields.is_empty() {
            write!(writer, "{}", message)?;
        } else if self.global_fields.is_empty() {
            write!(writer, "{} {}", message, fields_str)?;
        } else if fields_str.is_empty() {
            write!(writer, "{} {}", message, self.global_fields)?;
        } else {
            write!(writer, "{} {} {}", message, fields_str, self.global_fields)?;
        }
        writeln!(writer)
    }
}

/// owl-logger 自定义 JSON 格式化器
pub struct OwlJsonFormatter {
    pub(crate) show_target: bool,
    pub(crate) show_thread: bool,
    pub(crate) show_line_number: bool,
    pub(crate) time_format: String,
    pub(crate) use_utc: bool,
    pub(crate) global_fields: HashMap<String, String>,
}

struct JsonVisitor<'a> {
    map: &'a mut serde_json::Map<String, serde_json::Value>,
}

impl<'a> tracing::field::Visit for JsonVisitor<'a> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        let name = field.name();
        self.map.insert(
            name.to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        let name = field.name();
        self.map.insert(
            name.to_string(),
            serde_json::Value::Number(serde_json::Number::from(value)),
        );
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        let name = field.name();
        self.map.insert(
            name.to_string(),
            serde_json::Value::Number(serde_json::Number::from(value)),
        );
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        let name = field.name();
        self.map
            .insert(name.to_string(), serde_json::Value::Bool(value));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        let name = field.name();
        let val = if let Some(num) = serde_json::Number::from_f64(value) {
            serde_json::Value::Number(num)
        } else {
            serde_json::Value::String(value.to_string())
        };
        self.map.insert(name.to_string(), val);
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let name = field.name();
        let val_str = format!("{:?}", value);
        let cleaned = if val_str.starts_with('"') && val_str.ends_with('"') && val_str.len() >= 2 {
            val_str[1..val_str.len() - 1].to_string()
        } else {
            val_str
        };
        self.map
            .insert(name.to_string(), serde_json::Value::String(cleaned));
    }
}

impl<S, N> FormatEvent<S, N> for OwlJsonFormatter
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
        let mut log_obj = serde_json::Map::new();

        // 1. 时间戳
        let ts = if self.use_utc {
            chrono::Utc::now().format(&self.time_format).to_string()
        } else {
            chrono::Local::now().format(&self.time_format).to_string()
        };
        log_obj.insert("timestamp".to_string(), serde_json::Value::String(ts));

        // 2. 级别
        let level_str = event.metadata().level().to_string();
        log_obj.insert("level".to_string(), serde_json::Value::String(level_str));

        // 3. 来源模块
        if self.show_target {
            log_obj.insert(
                "target".to_string(),
                serde_json::Value::String(event.metadata().target().to_string()),
            );
        }

        // 4. 线程信息
        if self.show_thread {
            let thread_name = std::thread::current()
                .name()
                .unwrap_or("unnamed")
                .to_string();
            log_obj.insert("thread".to_string(), serde_json::Value::String(thread_name));
        }

        // 5. 源码行号
        if self.show_line_number {
            if let Some(file) = event.metadata().file() {
                log_obj.insert(
                    "file".to_string(),
                    serde_json::Value::String(file.to_string()),
                );
            }
            if let Some(line) = event.metadata().line() {
                log_obj.insert(
                    "line".to_string(),
                    serde_json::Value::Number(serde_json::Number::from(line)),
                );
            }
        }

        // 6. 收集 Event 字段
        let mut fields_map = serde_json::Map::new();
        let mut visitor = JsonVisitor {
            map: &mut fields_map,
        };
        event.record(&mut visitor);

        // 提取并移出 message
        if let Some(msg_val) = fields_map.remove("message") {
            log_obj.insert("message".to_string(), msg_val);
        } else {
            log_obj.insert(
                "message".to_string(),
                serde_json::Value::String("".to_string()),
            );
        }

        // 7. 事件自定义字段优先平铺；冲突时保留全部来源而不是覆盖。
        for (key, value) in fields_map {
            insert_json_field(&mut log_obj, &key, value);
        }

        // 8. Span 链上下文信息合并到顶层（包含 req_id 等）
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                let ext = span.extensions();
                if let Some(fields) = ext.get::<OwlSpanFields>() {
                    for (k, v) in &fields.0 {
                        insert_json_field(&mut log_obj, k, v.clone());
                    }
                }
            }
        }

        // 9. 全局字段合并到顶层
        for (k, v) in &self.global_fields {
            insert_json_field(&mut log_obj, k, serde_json::Value::String(v.clone()));
        }

        write!(writer, "{}", serde_json::Value::Object(log_obj))?;
        writeln!(writer)
    }
}

/// 用于文件输出的无色 Pretty 格式化器（禁用 ANSI 转义序列）
pub(crate) fn file_formatter(
    language: Language,
    config: &crate::config::OwlConfig,
) -> OwlFormatter {
    OwlFormatter {
        language,
        compact: false,
        show_target: config.show_target,
        show_thread: config.show_thread,
        show_line_number: config.show_line_number,
        enable_ansi: false, // 文件输出不带颜色
        time_format: config.time_format.clone(),
        use_utc: config.use_utc,
        global_fields: format_global_fields(&config.global_fields),
    }
}

/// 用于控制台输出的彩色 Pretty 格式化器
pub(crate) fn console_formatter(
    language: Language,
    config: &crate::config::OwlConfig,
) -> OwlFormatter {
    OwlFormatter {
        language,
        compact: false,
        show_target: config.show_target,
        show_thread: config.show_thread,
        show_line_number: config.show_line_number,
        enable_ansi: config.enable_ansi,
        time_format: config.time_format.clone(),
        use_utc: config.use_utc,
        global_fields: format_global_fields(&config.global_fields),
    }
}

/// 用于文件输出的无色 Compact 格式化器。
///
/// Compact 也必须经过 owl-logger 的字段访问器，保证全局字段与 Pretty/JSON 输出保持
/// 一致，不能回退到 tracing-subscriber 的默认格式化器。
pub(crate) fn file_compact_formatter(
    language: Language,
    config: &crate::config::OwlConfig,
) -> OwlFormatter {
    OwlFormatter {
        language,
        compact: true,
        show_target: config.show_target,
        show_thread: config.show_thread,
        show_line_number: config.show_line_number,
        enable_ansi: false,
        time_format: config.time_format.clone(),
        use_utc: config.use_utc,
        global_fields: format_global_fields(&config.global_fields),
    }
}

/// 用于控制台输出的 Compact 格式化器。
pub(crate) fn console_compact_formatter(
    language: Language,
    config: &crate::config::OwlConfig,
) -> OwlFormatter {
    OwlFormatter {
        language,
        compact: true,
        show_target: config.show_target,
        show_thread: config.show_thread,
        show_line_number: config.show_line_number,
        enable_ansi: config.enable_ansi,
        time_format: config.time_format.clone(),
        use_utc: config.use_utc,
        global_fields: format_global_fields(&config.global_fields),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_debug_quotes_removes_outer_quotes_only() {
        assert_eq!(strip_debug_quotes("\"req-001\"".to_string()), "req-001");
        assert_eq!(strip_debug_quotes("42".to_string()), "42");
        assert_eq!(strip_debug_quotes("\"\"".to_string()), "");
    }

    #[test]
    fn text_log_escaping_keeps_entries_single_line_and_readable() {
        assert_eq!(
            escape_log_text("line 1\nline 2").to_string(),
            "line 1\\nline 2"
        );
        assert_eq!(escape_log_text("a\"b\\c\r").to_string(), "a\\\"b\\\\c\\r");
        assert_eq!(escape_log_text("中文").to_string(), "中文");
    }

    #[test]
    fn global_fields_are_preformatted_in_stable_order() {
        let mut fields = HashMap::new();
        fields.insert("z".to_string(), "last".to_string());
        fields.insert("a".to_string(), "first\nline".to_string());

        assert_eq!(
            format_global_fields(&fields),
            "a=\"first\\nline\" z=\"last\""
        );
    }
}
