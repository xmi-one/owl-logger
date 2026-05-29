use std::fmt;
use std::collections::HashMap;

use owo_colors::OwoColorize;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

use crate::config::Language;
use crate::i18n::I18n;

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
        let ts = if self.use_utc {
            chrono::Utc::now().format(&self.time_format).to_string()
        } else {
            chrono::Local::now().format(&self.time_format).to_string()
        };
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

/// 用于 Pretty 格式的字段访问器，支持敏感数据脱敏
struct PrettyVisitor<'a> {
    message: &'a mut String,
    fields: &'a mut String,
    sensitive_keys: &'a [String],
}

impl<'a> tracing::field::Visit for PrettyVisitor<'a> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        let name = field.name();
        if name == "message" {
            *self.message = value.to_string();
            return;
        }
        if !self.fields.is_empty() {
            self.fields.push(' ');
        }
        if self.sensitive_keys.contains(&name.to_string()) {
            self.fields.push_str(&format!("{}=\"[MASKED]\"", name));
        } else {
            self.fields.push_str(&format!("{}=\"{}\"", name, value));
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let name = field.name();
        if name == "message" {
            let val_str = format!("{:?}", value);
            if val_str.starts_with('"') && val_str.ends_with('"') && val_str.len() >= 2 {
                *self.message = val_str[1..val_str.len() - 1].to_string();
            } else {
                *self.message = val_str;
            }
            return;
        }
        if !self.fields.is_empty() {
            self.fields.push(' ');
        }
        if self.sensitive_keys.contains(&name.to_string()) {
            self.fields.push_str(&format!("{}=\"[MASKED]\"", name));
        } else {
            self.fields.push_str(&format!("{}={:?}", name, value));
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
                        // 脱敏 Span 字段
                        let mut span_fields_map = serde_json::Map::new();
                        parse_span_fields(fields.fields.as_str(), &mut span_fields_map);
                        
                        let mut masked_span_fields = String::new();
                        for (k, v) in span_fields_map {
                            if !masked_span_fields.is_empty() {
                                masked_span_fields.push_str(", ");
                            }
                            let val_str = match v {
                                serde_json::Value::String(s) => s,
                                other => other.to_string(),
                            };
                            if self.sensitive_keys.contains(&k) {
                                masked_span_fields.push_str(&format!("{}=\"[MASKED]\"", k));
                            } else {
                                masked_span_fields.push_str(&format!("{}=\"{}\"", k, val_str));
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
            global_str.push_str(&format!("{}=\"{}\"", k, v));
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
            let colored_msg = match *level {
                Level::ERROR => full_msg.red().bold().to_string(),
                Level::WARN => full_msg.yellow().to_string(),
                Level::INFO => full_msg,
                Level::DEBUG | Level::TRACE => full_msg.dimmed().to_string(),
            };
            write!(writer, "{}", colored_msg)?;
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
        let val = if self.sensitive_keys.contains(&name.to_string()) {
            serde_json::Value::String("[MASKED]".to_string())
        } else {
            serde_json::Value::String(value.to_string())
        };
        self.map.insert(name.to_string(), val);
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        let name = field.name();
        let val = if self.sensitive_keys.contains(&name.to_string()) {
            serde_json::Value::String("[MASKED]".to_string())
        } else {
            serde_json::Value::Number(serde_json::Number::from(value))
        };
        self.map.insert(name.to_string(), val);
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        let name = field.name();
        let val = if self.sensitive_keys.contains(&name.to_string()) {
            serde_json::Value::String("[MASKED]".to_string())
        } else {
            serde_json::Value::Number(serde_json::Number::from(value))
        };
        self.map.insert(name.to_string(), val);
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        let name = field.name();
        let val = if self.sensitive_keys.contains(&name.to_string()) {
            serde_json::Value::String("[MASKED]".to_string())
        } else {
            serde_json::Value::Bool(value)
        };
        self.map.insert(name.to_string(), val);
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        let name = field.name();
        let val = if self.sensitive_keys.contains(&name.to_string()) {
            serde_json::Value::String("[MASKED]".to_string())
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
        let val = if self.sensitive_keys.contains(&name.to_string()) {
            serde_json::Value::String("[MASKED]".to_string())
        } else {
            let cleaned = if val_str.starts_with('"') && val_str.ends_with('"') && val_str.len() >= 2 {
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
            log_obj.insert("target".to_string(), serde_json::Value::String(event.metadata().target().to_string()));
        }

        // 4. 线程信息
        if self.show_thread {
            let thread_name = std::thread::current().name().unwrap_or("unnamed").to_string();
            log_obj.insert("thread".to_string(), serde_json::Value::String(thread_name));
        }

        // 5. 源码行号
        if self.show_line_number {
            if let Some(file) = event.metadata().file() {
                log_obj.insert("file".to_string(), serde_json::Value::String(file.to_string()));
            }
            if let Some(line) = event.metadata().line() {
                log_obj.insert("line".to_string(), serde_json::Value::Number(serde_json::Number::from(line)));
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
            log_obj.insert("message".to_string(), serde_json::Value::String("".to_string()));
        }

        // 7. Span 链上下文信息合并到顶层（包含 req_id 等）
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                let ext = span.extensions();
                if let Some(fields) = ext.get::<tracing_subscriber::fmt::FormattedFields<N>>() {
                    if !fields.is_empty() {
                        let mut span_fields = serde_json::Map::new();
                        parse_span_fields(fields.fields.as_str(), &mut span_fields);
                        for (k, mut v) in span_fields {
                            if self.sensitive_keys.contains(&k) {
                                v = serde_json::Value::String("[MASKED]".to_string());
                            }
                            log_obj.insert(k, v);
                        }
                    }
                }
            }
        }

        // 8. 全局字段合并到顶层
        for (k, v) in &self.global_fields {
            log_obj.insert(k.clone(), serde_json::Value::String(v.clone()));
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

/// 解析 Span 格式化字段字符串 (形如 `req_id="req-999" foo=bar`) 到 Map
fn parse_span_fields(s: &str, map: &mut serde_json::Map<String, serde_json::Value>) {
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() || c == ',' {
            chars.next();
            continue;
        }
        // 读取 Key
        let mut key = String::new();
        while let Some(&peek_c) = chars.peek() {
            if peek_c == '=' || peek_c.is_whitespace() {
                break;
            }
            key.push(peek_c);
            chars.next();
        }
        if chars.peek() == Some(&'=') {
            chars.next(); // 消费 '='
            // 读取 Value
            let mut val = String::new();
            if chars.peek() == Some(&'"') {
                chars.next(); // 消费 '"'
                for next_c in chars.by_ref() {
                    if next_c == '"' {
                        break;
                    }
                    val.push(next_c);
                }
            } else {
                while let Some(&peek_c) = chars.peek() {
                    if peek_c.is_whitespace() || peek_c == ',' {
                        break;
                    }
                    val.push(peek_c);
                    chars.next();
                }
            }
            map.insert(key, serde_json::Value::String(val));
        } else {
            chars.next();
        }
    }
}

/// 用于文件输出的无色 Pretty 格式化器（禁用 ANSI 转义序列）
pub(crate) fn file_formatter(language: Language, config: &crate::config::OwlConfig) -> OwlFormatter {
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
pub(crate) fn console_formatter(language: Language, config: &crate::config::OwlConfig) -> OwlFormatter {
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
