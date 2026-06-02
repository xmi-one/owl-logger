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

const MASKED: &str = "[MASKED]";

fn is_sensitive_key(sensitive_keys: &[String], field_name: &str) -> bool {
    sensitive_keys
        .iter()
        .any(|key| key.eq_ignore_ascii_case(field_name))
}

/// 去除 `{:?}` 调试格式化产生的最外层引号（如 `"req-001"` -> `req-001`）
fn strip_debug_quotes(s: String) -> String {
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len() - 1].to_string()
    } else {
        s
    }
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

/// 将结构化 span 字段值转换为展示字符串（Pretty 格式用）
fn span_value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn mask_string_if_sensitive(
    sensitive_keys: &[String],
    field_name: &str,
    value: impl Into<String>,
) -> serde_json::Value {
    if is_sensitive_key(sensitive_keys, field_name) {
        serde_json::Value::String(MASKED.to_string())
    } else {
        serde_json::Value::String(value.into())
    }
}

/// owl-logger 自定义格式化器（Pretty / 文本格式）
pub struct OwlFormatter {
    pub(crate) language: Language,
    pub(crate) show_target: bool,
    pub(crate) show_thread: bool,
    pub(crate) show_line_number: bool,
    pub(crate) enable_ansi: bool,
    pub(crate) time_format: String,
    pub(crate) use_utc: bool,
    pub(crate) global_fields: HashMap<String, String>,
    pub(crate) sensitive_keys: Vec<String>,
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

/// 用于 Pretty 格式的字段访问器，支持敏感数据脱敏
struct PrettyVisitor<'a> {
    message: &'a mut String,
    fields: &'a mut String,
    sensitive_keys: &'a [String],
}

impl<'a> tracing::field::Visit for PrettyVisitor<'a> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        use std::fmt::Write as _;
        let name = field.name();
        if name == "message" {
            self.message.clear();
            self.message.push_str(value);
            return;
        }
        if !self.fields.is_empty() {
            self.fields.push(' ');
        }
        if is_sensitive_key(self.sensitive_keys, name) {
            let _ = write!(self.fields, "{}=\"{}\"", name, MASKED);
        } else {
            let _ = write!(self.fields, "{}=\"{}\"", name, value);
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        let name = field.name();
        if name == "message" {
            let val_str = format!("{:?}", value);
            *self.message = strip_debug_quotes(val_str);
            return;
        }
        if !self.fields.is_empty() {
            self.fields.push(' ');
        }
        if is_sensitive_key(self.sensitive_keys, name) {
            let _ = write!(self.fields, "{}=\"{}\"", name, MASKED);
        } else {
            let _ = write!(self.fields, "{}={:?}", name, value);
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
        // 时间 | 级别 |
        self.write_timestamp(&mut writer)?;
        self.write_separator(&mut writer)?;
        self.write_level(&mut writer, event.metadata().level())?;
        self.write_separator(&mut writer)?;

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
                        let mut masked_span_fields = String::new();
                        for (k, v) in &fields.0 {
                            if !masked_span_fields.is_empty() {
                                masked_span_fields.push_str(", ");
                            }
                            if is_sensitive_key(&self.sensitive_keys, k) {
                                masked_span_fields.push_str(&format!("{}=\"{}\"", k, MASKED));
                            } else {
                                masked_span_fields.push_str(&format!(
                                    "{}=\"{}\"",
                                    k,
                                    span_value_to_string(v)
                                ));
                            }
                        }

                        if !masked_span_fields.is_empty() {
                            write!(writer, "{{{}}}", masked_span_fields)?;
                        }
                    }
                }
                first = false;
            }
            if !first {
                self.write_separator(&mut writer)?;
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
            self.write_separator(&mut writer)?;
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
            self.write_separator(&mut writer)?;
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
                    self.write_separator(&mut writer)?;
                }
            }
        }

        // 提取日志消息与自定义字段
        let mut message = String::new();
        let mut fields_str = String::new();
        let mut visitor = PrettyVisitor {
            message: &mut message,
            fields: &mut fields_str,
            sensitive_keys: &self.sensitive_keys,
        };
        event.record(&mut visitor);

        // 拼接全局字段
        let mut global_str = String::new();
        for (k, v) in &self.global_fields {
            if !global_str.is_empty() {
                global_str.push(' ');
            }
            if is_sensitive_key(&self.sensitive_keys, k) {
                global_str.push_str(&format!("{}=\"{}\"", k, MASKED));
            } else {
                global_str.push_str(&format!("{}=\"{}\"", k, v));
            }
        }

        let full_msg = if global_str.is_empty() {
            if fields_str.is_empty() {
                message
            } else {
                format!("{} {}", message, fields_str)
            }
        } else if fields_str.is_empty() {
            format!("{} {}", message, global_str)
        } else {
            format!("{} {} {}", message, fields_str, global_str)
        };

        if self.enable_ansi {
            let level = event.metadata().level();
            match *level {
                Level::ERROR => write!(writer, "{}", full_msg.red().bold())?,
                Level::WARN => write!(writer, "{}", full_msg.yellow())?,
                Level::INFO => write!(writer, "{}", full_msg)?,
                Level::DEBUG | Level::TRACE => write!(writer, "{}", full_msg.dimmed())?,
            }
        } else {
            write!(writer, "{}", full_msg)?;
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
    pub(crate) sensitive_keys: Vec<String>,
}

struct JsonVisitor<'a> {
    map: &'a mut serde_json::Map<String, serde_json::Value>,
    sensitive_keys: &'a [String],
}

impl<'a> tracing::field::Visit for JsonVisitor<'a> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        let name = field.name();
        let val = mask_string_if_sensitive(self.sensitive_keys, name, value);
        self.map.insert(name.to_string(), val);
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        let name = field.name();
        let val = if is_sensitive_key(self.sensitive_keys, name) {
            serde_json::Value::String(MASKED.to_string())
        } else {
            serde_json::Value::Number(serde_json::Number::from(value))
        };
        self.map.insert(name.to_string(), val);
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        let name = field.name();
        let val = if is_sensitive_key(self.sensitive_keys, name) {
            serde_json::Value::String(MASKED.to_string())
        } else {
            serde_json::Value::Number(serde_json::Number::from(value))
        };
        self.map.insert(name.to_string(), val);
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        let name = field.name();
        let val = if is_sensitive_key(self.sensitive_keys, name) {
            serde_json::Value::String(MASKED.to_string())
        } else {
            serde_json::Value::Bool(value)
        };
        self.map.insert(name.to_string(), val);
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        let name = field.name();
        let val = if is_sensitive_key(self.sensitive_keys, name) {
            serde_json::Value::String(MASKED.to_string())
        } else if let Some(num) = serde_json::Number::from_f64(value) {
            serde_json::Value::Number(num)
        } else {
            serde_json::Value::String(value.to_string())
        };
        self.map.insert(name.to_string(), val);
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let name = field.name();
        let val_str = format!("{:?}", value);
        let val = if is_sensitive_key(self.sensitive_keys, name) {
            serde_json::Value::String(MASKED.to_string())
        } else {
            let cleaned =
                if val_str.starts_with('"') && val_str.ends_with('"') && val_str.len() >= 2 {
                    val_str[1..val_str.len() - 1].to_string()
                } else {
                    val_str
                };
            serde_json::Value::String(cleaned)
        };
        self.map.insert(name.to_string(), val);
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

        // 6. 收集并脱敏 Event 字段
        let mut fields_map = serde_json::Map::new();
        let mut visitor = JsonVisitor {
            map: &mut fields_map,
            sensitive_keys: &self.sensitive_keys,
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

        // 7. Span 链上下文信息合并到顶层（包含 req_id 等）
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                let ext = span.extensions();
                if let Some(fields) = ext.get::<OwlSpanFields>() {
                    for (k, v) in &fields.0 {
                        let value = if is_sensitive_key(&self.sensitive_keys, k) {
                            serde_json::Value::String(MASKED.to_string())
                        } else {
                            v.clone()
                        };
                        log_obj.insert(k.clone(), value);
                    }
                }
            }
        }

        // 8. 全局字段合并到顶层
        for (k, v) in &self.global_fields {
            log_obj.insert(
                k.clone(),
                mask_string_if_sensitive(&self.sensitive_keys, k, v.clone()),
            );
        }

        // 9. 如果还有其他自定义 fields，平铺在顶层
        for (k, v) in fields_map {
            log_obj.insert(k, v);
        }

        if let Ok(serialized) = serde_json::to_string(&log_obj) {
            write!(writer, "{}", serialized)?;
        }
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
        show_target: config.show_target,
        show_thread: config.show_thread,
        show_line_number: config.show_line_number,
        enable_ansi: false, // 文件输出不带颜色
        time_format: config.time_format.clone(),
        use_utc: config.use_utc,
        global_fields: config.global_fields.clone(),
        sensitive_keys: config.sensitive_keys.clone(),
    }
}

/// 用于控制台输出的彩色 Pretty 格式化器
pub(crate) fn console_formatter(
    language: Language,
    config: &crate::config::OwlConfig,
) -> OwlFormatter {
    OwlFormatter {
        language,
        show_target: config.show_target,
        show_thread: config.show_thread,
        show_line_number: config.show_line_number,
        enable_ansi: config.enable_ansi,
        time_format: config.time_format.clone(),
        use_utc: config.use_utc,
        global_fields: config.global_fields.clone(),
        sensitive_keys: config.sensitive_keys.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_key_matching_is_case_insensitive() {
        let keys = vec!["token".to_string(), "api_key".to_string()];

        assert!(is_sensitive_key(&keys, "Token"));
        assert!(is_sensitive_key(&keys, "API_KEY"));
        assert!(!is_sensitive_key(&keys, "user"));
    }

    #[test]
    fn global_field_values_are_masked_when_sensitive() {
        let keys = vec!["authorization".to_string()];

        assert_eq!(
            mask_string_if_sensitive(&keys, "Authorization", "Bearer secret"),
            serde_json::Value::String(MASKED.to_string())
        );
        assert_eq!(
            mask_string_if_sensitive(&keys, "env", "prod"),
            serde_json::Value::String("prod".to_string())
        );
    }

    #[test]
    fn strip_debug_quotes_removes_outer_quotes_only() {
        assert_eq!(strip_debug_quotes("\"req-001\"".to_string()), "req-001");
        assert_eq!(strip_debug_quotes("42".to_string()), "42");
        assert_eq!(strip_debug_quotes("\"\"".to_string()), "");
    }
}
