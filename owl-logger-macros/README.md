# 🦉 owl-logger

**开箱即用、生产级的 Rust 日志库** — 基于 `tracing` 生态构建，借鉴 Python `loguru` 产品理念。

[![Crates.io](https://img.shields.io/crates/v/owl-logger.svg)](https://crates.io/crates/owl-logger)
[![Docs.rs](https://docs.rs/owl-logger/badge.svg)](https://docs.rs/owl-logger)
[![License](https://img.shields.io/crates/l/owl-logger.svg)](LICENSE)

## ✨ 特性

- 🚀 **一行初始化** — 零配置即可开始使用
- 🎨 **彩色输出** — 控制台日志带有颜色高亮，自动对齐
- 🔄 **动态调整** — 运行时动态获取、更改日志级别与过滤规则，无需重启服务
- 🔒 **敏感数据脱敏** — 内置敏感关键字检测（如 `password`, `token`），输出时自动脱敏为 `"[MASKED]"`
- 🗜️ **后台异步压缩** — 支持大小轮转后，在后台线程自动把旧日志打包压缩为 `.gz` 文件
- 🧹 **保留期自动清理** — 支持设定日志保留天数，在独立后台线程中自动清理过期的历史日志文件
- ⏱️ **函数监控宏** — `#[monitor]` 过程宏自动记录入参、返回值和耗时，**若返回 Err 自动升级为 ERROR 级别**并打印错误细节
- 💥 **崩溃堆栈捕获** — 接管 panic hook，捕获崩溃时能够打印堆栈回溯（`Backtrace`）
- 🔗 **上下文追踪** — 基于 `tracing::Span` 自动注入 `req_id`，支持同步/异步环境
- 📊 **结构化 JSON** — 提供高度定制的扁平化 JSON 格式，便于 ELK / Datadog 等日志分析系统归档
- 🧹 **优雅自动清理** — 利用 Rust 的 `Drop` 特征自动 flush，无需手动 `cleanup` 避免丢失日志
- 🔌 **生态兼容** — 自动桥接 `log` crate 生态，接管所有依赖库的日志

## 📦 安装

```toml
[dependencies]
owl-logger = "0.2.1"
```

## 🚀 快速开始

### 零配置初始化

```rust
fn main() {
    let _guard = owl_logger::init();

    owl_logger::info!("Hello from owl-logger! 🦉");
    owl_logger::warn!(user = "alice", "Something needs attention");
    owl_logger::error!("Oops, something went wrong");
}
```

### 自定义配置

```rust
use owl_logger::{Language, LogLevel, RotationPolicy};

fn main() {
    let _guard = owl_logger::builder()
        .file_name("my_app")
        .log_dir("logs")
        .language(Language::Zh)
        .level(LogLevel::Info)
        .rotation(RotationPolicy::SizeMB(10)) // 每 10MB 轮转并压缩
        .global_field("env", "production")     // 添加全局字段
        .sensitive_key("api_key")             // 添加脱敏词
        .retention_days(7)                    // 日志保留 7 天
        .show_line_number(true)
        .init();

    owl_logger::info!("🦉 开始工作！");
}
```

## 📖 功能详解

### 1. 运行时动态调整级别/过滤规则

利用 `reload` 句柄，可以在不重启服务器的情况下，动态控制输出级别：

```rust
// 动态修改全局级别为 Debug
owl_logger::set_level(owl_logger::LogLevel::Debug).unwrap();

// 动态设置复杂过滤规则
owl_logger::set_filter("info,my_crate=trace").unwrap();

// 获取当前生效的过滤器规则
let current_filter = owl_logger::get_filter().unwrap();
```

### 2. 函数监控宏与异常升级

使用 `#[owl_logger::monitor]` 标记函数。宏会在**编译期**解析参数，并结合 **Autoref 特化** 机制在运行期侦测返回值：
* 函数若成功执行，输出 `INFO` 日志。
* **如果函数返回 `Result::Err`，退出日志将自动提至 `ERROR` 级别并以红色高亮打印出错误详情。**

```rust
use owl_logger::monitor;

#[monitor]
fn perform_action(user: &str) -> Result<String, String> {
    if user == "admin" {
        Ok("Welcome".to_string())
    } else {
        Err("Permission Denied".to_string())
    }
}
// 1. 调用 perform_action("admin") ➔ 输出 INFO: ← exiting ... returned Ok("Welcome")
// 2. 调用 perform_action("guest") ➔ 输出 ERROR: ← exiting ... — ERROR: "Permission Denied"
```

### 3. 敏感数据安全脱敏 (PII Masking)

在控制台和 JSON 格式输出中，只要日志字段名命中脱敏关键字列表，其具体数值将被自动脱敏过滤：

```rust
// 默认脱敏字段包括 password, token, secret, authorization, credit_card
tracing::warn!(
    password = "secret-plain-text",
    user = "bob",
    "用户密码登录尝试"
);
// 输出中将以: password="[MASKED]" user="bob" 形式呈现，防泄漏安全合规
```

### 4. 请求上下文追踪 (MDC)

利用 `tracing::Span` 进行线程与异步协程间安全的请求 ID 跟踪：

```rust
fn main() {
    let _guard = owl_logger::init();

    // 同步上下文
    {
        let _ctx = owl_logger::context::with_request_id("req-001");
        owl_logger::info!("处理订单"); // 日志自动带上 req_id="req-001"
    }

    // 异步上下文
    // some_async_fn()
    //     .instrument(owl_logger::context::request_span("req-002"))
    //     .await;
}
```

---

## ⚙️ 配置项说明

在 `OwlLoggerBuilder` 中支持链式调用如下配置：

| 方法 | 默认值 | 说明 |
|:---|:---|:---|
| `.file_name(name)` | `"app"` | 日志文件名前缀 |
| `.log_dir(dir)` | `"logs"` | 日志保存目录 |
| `.level(level)` | `Info` | 最低日志过滤级别 |
| `.language(lang)` | `En` | 系统提示词语言（En / Zh） |
| `.format(fmt)` | `Pretty` | 输出格式（Pretty / Compact / Json） |
| `.rotation(policy)` | `Daily` | 文件轮转策略（Daily / Hourly / SizeMB(mb) / Never） |
| `.global_field(k, v)` | - | 全局属性字段，会自动平铺附加在每条日志中 |
| `.sensitive_key(key)` | - | 追加单个脱敏词（如密码、Token 字段名） |
| `.sensitive_keys(keys)` | - | 重新覆盖脱敏词列表 |
| `.retention_days(days)` | `Some(7)` | 日志过期天数（后台定期清理，默认保留 7 天） |
| `.buffered_lines_limit(n)` | `120_000` | 异步非阻塞缓冲队列行数限制 |
| `.lossy(bool)` | `true` | 队列写满时是否丢弃（`false` 会阻塞当前线程保证防丢失） |
| `.console(bool)` | `true` | 是否启用控制台（Stderr）输出 |
| `.file(bool)` | `true` | 是否启用文件输出 |
| `.ansi(bool)` | `true` | 是否对控制台输出启用 ANSI 终端彩色着色 |
| `.show_target(bool)` | `true` | 日志是否显示目标模块路径 |
| `.show_thread(bool)` | `false` | 日志是否显示线程名称 |
| `.show_line_number(bool)` | `false` | 日志是否显示文件名与行号 |
| `.time_format(format)` | `"%Y-%m-%d %H:%M:%S%.3f"` | 时间戳的 Chrono 格式化字符串 |
| `.utc(bool)` | `false` | 是否强制使用 UTC 时区（默认本地时区） |
| `.max_files(n)` | `None` | 最大历史保留文件数限制（按个数限制） |
| `.catch_panic(bool)` | `true` | 是否自动接管 Panic 并附带 Backtrace 堆栈输出到日志中 |

---

## 📄 License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
