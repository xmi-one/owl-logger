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
- 🗜️ **可靠日志压缩** — 支持大小轮转后自动把旧日志打包压缩为 `.gz` 文件
- 🧹 **保留期自动清理** — 支持设定日志保留天数，在独立后台线程中自动清理过期的历史日志文件
- 🚨 **按级别分文件** — 可将达到指定级别（如 `ERROR`）的日志额外单独写入 `{file}.error.log`，便于运维定位
- 🛰️ **OpenTelemetry/OTLP 导出** — 通过 `otlp` feature 将追踪 span 导出到 Jaeger / Tempo / OTel Collector（OTLP/HTTP，无需 Tokio 运行时）
- ⏱️ **函数监控宏** — `#[monitor]` 过程宏自动记录入参、返回值和耗时，**若返回 Err 自动升级为 ERROR 级别**并打印错误细节
- 💥 **崩溃堆栈捕获** — 接管 panic hook，捕获崩溃时能够打印堆栈回溯（`Backtrace`）
- 🔗 **上下文追踪** — 基于 `tracing::Span` 自动注入 `req_id`，支持同步/异步环境
- 📊 **结构化 JSON** — 提供高度定制的扁平化 JSON 格式，便于 ELK / Datadog 等日志分析系统归档
- 🧹 **优雅自动清理** — 利用 Rust 的 `Drop` 特征自动 flush，无需手动 `cleanup` 避免丢失日志
- 📉 **丢失可观测** — 有损队列模式下可通过 `OwlGuard::dropped_lines()` 获取各输出通道的丢弃计数
- 🔌 **生态兼容** — 自动桥接 `log` crate 生态，接管所有依赖库的日志

## 📦 安装

```toml
[dependencies]
owl-logger = "0.2.4"
```

MSRV：Rust 1.88。

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

也可以用环境变量生成配置：

```rust
let _guard = owl_logger::try_init_from_env().unwrap();
```

支持 `OWL_LOG_LEVEL`、`OWL_LOG_FORMAT`、`OWL_LOG_DIR`、`OWL_LOG_FILE`。
若设置了有效的 `RUST_LOG`，它会优先于 `OWL_LOG_LEVEL` 和 builder 的基础 level 生效。

### 2. 函数监控宏与异常升级

使用 `#[owl_logger::monitor]` 标记函数。宏会在**编译期**解析参数，并结合 **Autoref 特化** 机制在运行期侦测返回值：
* 函数若成功执行，输出 `INFO` 日志。
* **如果函数返回 `Result::Err`，退出日志将自动提至 `ERROR` 级别并以红色高亮打印出错误详情。**

```rust
use owl_logger::monitor;

#[monitor(slow_ms = 200)]
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

宏支持的参数：

| 参数 | 说明 |
|:---|:---|
| `level = "debug"` | 监控日志级别（默认 `info`） |
| `skip(a, b)` | 将指定参数脱敏为 `[REDACTED]` |
| `skip_all` | 不记录任何参数，适用于参数未实现 `Debug` 或整体敏感的函数 |
| `skip_return` | 不记录返回值；同时关闭 Result::Err 自动升级，适用于返回值未实现 `Debug` 或敏感返回值 |
| `slow_ms = 200` | 超过该毫秒数时以 WARN 级别标记 `SLOW` |
| `span`（或 `span = true`） | 为函数体建立 `tracing::Span`，使函数**内部**的日志自动带上以函数名命名的上下文 |

> 性能说明：当 `monitor` 目标日志被完全过滤时（例如 `set_filter("monitor=off")`），参数 `Debug` 格式化与进入/退出日志逻辑都会被跳过，实现近似零开销。

```rust
// span 化：handle 内部的所有日志都会自动归属于 handle 这个 span
#[monitor(span, slow_ms = 500)]
fn handle(req_id: u64) {
    tracing::info!("processing"); // 自动带上 handle 上下文
}
```

### 3. 敏感数据安全脱敏 (PII Masking)

在控制台和 JSON 格式输出中，只要日志字段名（大小写无关、包含匹配）命中脱敏关键字列表，其具体数值将被自动脱敏过滤：

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
| `.file_name(name)` | `"app"` | 日志文件名前缀（必须是单个文件名，不能包含路径） |
| `.log_dir(dir)` | `"logs"` | 日志保存目录 |
| `.level(level)` | `Info` | 最低日志过滤级别 |
| `.language(lang)` | `En` | 系统提示词语言（En / Zh） |
| `.format(fmt)` | `Pretty` | 输出格式（Pretty / Compact / Json） |
| `.rotation(policy)` | `Daily` | 文件轮转策略（Daily / Hourly / SizeMB(mb) / Never） |
| `.global_field(k, v)` | - | 全局属性字段，会自动平铺附加在每条日志中 |
| `.sensitive_key(key)` | - | 追加单个脱敏词（如密码、Token 字段名） |
| `.sensitive_keys(keys)` | - | 重新覆盖脱敏词列表 |
| `.retention_days(days)` | `Some(7)` | 日志过期天数（后台定期清理，默认保留 7 天） |
| `.error_file(level)` | `None` | 额外写入 `{file}.{level}.log`，仅记录达到或严重于该级别的日志 |
| `.otlp_endpoint(url)` | `None` | OTLP/HTTP 导出端点（需启用 `otlp` feature） |
| `.otlp_service_name(name)` | `file_name` | OTLP 上报的 `service.name`（需启用 `otlp` feature） |
| `.buffered_lines_limit(n)` | `120_000` | 异步非阻塞缓冲队列行数限制 |
| `.lossy(bool)` | `true` | 队列写满时是否丢弃（`false` 会阻塞当前线程保证防丢失）；有损模式可通过 Guard 查询丢弃计数 |
| `.console(bool)` | `true` | 是否启用控制台（Stderr）输出 |
| `.file(bool)` | `true` | 是否启用文件输出 |
| `.ansi(bool)` | `true` | 是否对控制台输出启用 ANSI 终端彩色着色 |
| `.show_target(bool)` | `true` | 日志是否显示目标模块路径 |
| `.show_thread(bool)` | `false` | 日志是否显示线程名称 |
| `.show_line_number(bool)` | `false` | 日志是否显示文件名与行号 |
| `.time_format(format)` | `"%Y-%m-%d %H:%M:%S%.3f"` | 时间戳的 Chrono 格式化字符串 |
| `.utc(bool)` | `false` | 是否强制使用 UTC 时区（默认本地时区） |
| `.max_files(n)` | `None` | 最大日志文件数（含当前活跃文件；`0` 表示不限） |
| `.catch_panic(bool)` | `false` | 是否接管进程级 Panic hook 并附带 Backtrace；建议仅在应用入口显式启用 |

环境变量初始化支持：

| 变量 | 可选值 / 说明 |
|:---|:---|
| `OWL_LOG_LEVEL` | `trace` / `debug` / `info` / `warn` / `error` |
| `OWL_LOG_FORMAT` | `pretty` / `compact` / `json` |
| `OWL_LOG_DIR` | 日志保存目录 |
| `OWL_LOG_FILE` | 日志文件名前缀 |
| `RUST_LOG` | tracing 的完整过滤规则，优先于基础 level 配置 |

可在服务运行期间采集有损队列的丢弃计数：

```rust
let dropped = _guard.dropped_lines();
if dropped.total() > 0 {
    eprintln!("dropped log output lines: {dropped:?}");
}
```

### 5. 按级别分文件（error.log）

将错误日志单独落盘，便于运维快速排障：

```rust
use owl_logger::LogLevel;

let _guard = owl_logger::builder()
    .file_name("app")
    .error_file(LogLevel::Error) // 额外生成 app.error.log，仅含 ERROR
    .init();
```

`error_file(LogLevel::Warn)` 则会生成 `app.warn.log`，同时包含 WARN 与 ERROR。该独立文件与主文件共享相同的轮转、压缩与清理策略。

### 6. OpenTelemetry / OTLP 分布式追踪

启用 `otlp` feature 后，可将 `tracing` 的 span 通过 OTLP/HTTP 导出到 Jaeger、Tempo、OpenTelemetry Collector 等后端。采用阻塞式 reqwest 传输，**无需** 应用提供 Tokio 运行时。

```toml
[dependencies]
owl-logger = { version = "0.2.4", features = ["otlp"] }
```

```rust
let _guard = owl_logger::builder()
    .otlp_endpoint("http://localhost:4318/v1/traces")
    .otlp_service_name("my-service")
    .init();

// 配合 #[monitor(span)] 或 tracing span，调用链将作为 trace 上报
```

可先用 Jaeger all-in-one 快速体验：

```bash
docker run --rm -p 4318:4318 -p 16686:16686 jaegertracing/all-in-one
cargo run --example otlp -p owl-logger --features otlp
# 打开 http://localhost:16686 查看名为 owl-demo 的服务追踪
```

---

## 📄 License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
