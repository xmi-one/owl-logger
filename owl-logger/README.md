# 🦉 owl-logger

**开箱即用、生产级的 Rust 日志库** — 基于 `tracing` 生态构建，对齐 Python `yyds-logger` 的生产级稳健性理念。

[![Crates.io](https://img.shields.io/crates/v/owl-logger.svg)](https://crates.io/crates/owl-logger)
[![Docs.rs](https://docs.rs/owl-logger/badge.svg)](https://docs.rs/owl-logger)
[![License](https://img.shields.io/crates/l/owl-logger.svg)](LICENSE)

## ✨ 特性

- 🚀 **一行初始化** — 零配置即可开始使用，启动期快速校验文件可写性
- 🎨 **通道独立格式与级别** — 支持控制台保留彩色 `Pretty` 的同时文件输出结构化 `Json`，并可为控制台与文件分别设定过滤级别
- 🧾 **单行文本日志** — Pretty / Compact 输出自动转义换行、引号与控制字符，避免单条事件被拆成多行
- 🔄 **动态调整** — 运行时动态获取、更改日志级别与过滤规则，无需重启服务
- 🗜️ **无阻塞大小轮转压缩** — `SizeMB` 轮转在微秒级切换新文件，由独立有序后台线程执行 Gzip 压缩，不阻塞日志写入
- 🧹 **无竞态保留期清理** — 支持设定日志保留天数（传入 `0` 可关闭），后台清理与轮转写入共享目录锁，幂等安全
- 🚨 **按级别分文件** — 可将达到指定级别（如 `ERROR`）的日志单独写入 `{file}.error.log`，同时完整保留常规 `info_span!` 上下文
- 🛰️ **OpenTelemetry/OTLP 导出** — 通过 `otlp` feature（内置 `rustls` HTTPS 支持）将追踪 span 导出到 Jaeger / Tempo / OTel Collector，无需 Tokio 运行时
- ⏱️ **函数监控宏** — `#[monitor]` 自动记录入参、返回值和耗时，支持大对象截断保护（`max_len`）、编译期 `skip` 校验，返回 `Err` 自动升级为 `ERROR`
- 💥 **崩溃堆栈捕获与紧急刷盘** — 接管 panic hook 强制捕获 `Backtrace` 并在退出前同步刷盘，即使 `panic = "abort"` 也不丢崩溃日志
- 🔗 **上下文追踪** — 基于 `tracing::Span` 自动注入 `req_id` 与原生类型字段，支持同步/异步环境
- 📉 **全链路可观测与主动刷盘** — 支持运行期 `owl_logger::flush()` / `guard.flush()`，并通过 `dropped_lines()` 与 `sink_errors()` 观测队列丢弃与磁盘 I/O 错误
- 🔌 **生态兼容** — 自动桥接 `log` crate 生态，接管所有三方依赖库日志

## 📦 安装

```toml
[dependencies]
owl-logger = "0.4.0"
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

### 生产推荐配置（控制台彩色 + 文件 JSON + 有界阻塞背压）

```rust
use std::time::Duration;
use owl_logger::{Language, LogLevel, OutputFormat, RotationPolicy};

fn main() {
    let guard = owl_logger::builder()
        .file_name("my_app")
        .log_dir("logs")
        .language(Language::Zh)
        .console_format(OutputFormat::Pretty) // 终端保持彩色易读
        .file_format(OutputFormat::Json)      // 文件落盘结构化 JSON
        .console_level(LogLevel::Warn)        // 终端仅看告警与错误
        .file_level(LogLevel::Info)           // 文件保留完整业务日志
        .error_file(LogLevel::Error)          // 独立生成 my_app.error.log
        .rotation(RotationPolicy::SizeMB(50)) // 每 50MB 轮转并在后台压缩为 .gz
        .retention_days(7)                    // 历史归档保留 7 天（传 0 表示永久保留）
        .lossy(false)                         // 队列满时背压等待
        .queue_timeout(Duration::from_secs(1))// 最多阻塞 1 秒，超时降级防卡死
        .global_field("env", "production")
        .catch_panic(true)
        .init();

    owl_logger::info!("🦉 服务启动！");

    // 运行期可随时主动排空缓冲队列刷盘，无需销毁 guard
    guard.flush().unwrap();
}
```

## 📖 功能详解

### 1. 运行时动态调整级别/过滤规则

```rust
// 动态修改全局级别为 Debug
owl_logger::set_level(owl_logger::LogLevel::Debug).unwrap();

// 动态设置复杂过滤规则
owl_logger::set_filter("info,my_crate=trace").unwrap();

// 获取当前生效的过滤器规则
let current_filter = owl_logger::get_filter().unwrap();
```

也可以通过环境变量初始化配置：

```rust
let _guard = owl_logger::try_init_from_env().unwrap();
```

### 2. 函数监控宏与异常升级

使用 `#[owl_logger::monitor]` 标记同步或异步函数：
* 函数正常返回时按配置级别（默认 `INFO`）输出；
* 若返回 `Result::Err`，退出日志自动升级为 `ERROR` 级别并打印错误详情；
* 完美兼容 `.collect()`、`.into()`、`Default::default()`、`impl Trait` 等依赖返回类型推导的函数体。

```rust
use owl_logger::monitor;

#[monitor(span, slow_ms = 200, skip(password), max_len = 512)]
fn perform_action(user: &str, password: &str) -> Result<String, String> {
    let _ = password;
    if user == "admin" {
        Ok("Welcome".into())
    } else {
        Err("Permission Denied".into())
    }
}
```

宏支持的参数：

| 参数 | 说明 |
|:---|:---|
| `level = "debug"` | 监控日志级别（默认 `info`） |
| `skip(self, a, b)` | 省略指定参数（支持 `self`），输出为 `[REDACTED]`（编译期校验参数名，防止拼写错误泄密） |
| `skip_all` | 不记录任何参数，适用于参数未实现 `Debug` 或不应输出参数的函数 |
| `skip_return` | 不记录返回值；同时关闭 `Result::Err` 自动升级 |
| `slow_ms = 200` | 超过该毫秒数时以 `WARN` 级别标记 `SLOW` |
| `max_len = 1024` | 单个入参或返回值 `Debug` 文本的最大字节长度，超出短路截断并追加 `...`（默认 `1024`，`0` 表示不限） |
| `span`（或 `span = true`） | 为函数建立 `tracing::Span`，使进入/退出/异常日志与函数体内部日志全部归属于该 Span 上下文 |
| `target = "my_target"` | 自定义 `tracing` 事件与 Span 的目标模块名（默认 `"monitor"`） |
| `module_target` | 使用调用点所在模块路径（`module_path!()`）作为目标名，便于配合 `my_crate=info` 模块级过滤规则 |

### 3. 请求上下文追踪 (MDC)

```rust
fn main() {
    let _guard = owl_logger::init();

    // 同步请求 ID 上下文
    {
        let _ctx = owl_logger::context::with_request_id("req-001");
        owl_logger::info!("处理订单"); // 自动带上 req_id="req-001"
    }

    // 自定义多字段上下文 Span（JSON 下保留数值、布尔原生类型）
    {
        let span = owl_logger::context_span!("order", order_id = "ORD-1", attempt = 1, retryable = true);
        let _entered = span.enter();
        owl_logger::info!("扣减库存");
    }
}
```

---

## ⚙️ 配置项说明

| 方法 | 默认值 | 说明 |
|:---|:---|:---|
| `.file_name(name)` | `"app"` | 日志文件名前缀（单个文件名组件，不含路径分隔符） |
| `.log_dir(dir)` | `"logs"` | 日志保存目录 |
| `.level(level)` | `Info` | 全局基础最低日志级别 |
| `.console_level(level)` | `None` | 控制台独立最低级别 |
| `.file_level(level)` | `None` | 主文件独立最低级别 |
| `.language(lang)` | `En` | 系统提示词语言（`En` / `Zh`） |
| `.format(fmt)` | `Pretty` | 默认输出格式（`Pretty` / `Compact` / `Json`） |
| `.console_format(fmt)` | `None` | 控制台独立输出格式（未设置时回退 `.format`） |
| `.file_format(fmt)` | `None` | 文件独立输出格式（未设置时回退 `.format`） |
| `.rotation(policy)` | `Daily` | 轮转策略（`SizeMB(mb)` / `Daily` / `Hourly` / `Never`） |
| `.global_field(k, v)` | - | 全局属性字段，稳定排序并平铺附加到每条日志 |
| `.retention_days(days)` | `Some(7)` | 日志保留天数（传 `0` 关闭按天自动清理） |
| `.max_files(n)` | `None` | 最大日志文件数（含当前活跃文件；`0` 表示不限） |
| `.error_file(level)` | `None` | 额外写入 `{file}.{level}.log`（完整保留常规 Span 上下文） |
| `.process_isolation(bool)` | `false` | 多进程文件名隔离，自动附带当前 PID（如 `app.pid12345.log`）防跨进程轮转冲突 |
| `.otlp_endpoint(url)` | `None` | OTLP/HTTP(S) 导出端点（需启用 `otlp` feature） |
| `.otlp_service_name(name)` | `file_name` | OTLP 上报的 `service.name`（需启用 `otlp` feature） |
| `.buffered_lines_limit(n)` | `120_000` | 异步缓冲队列容量上限 |
| `.lossy(bool)` | `true` | 队列满时是否立即丢弃（`false` 时阻塞等待） |
| `.queue_timeout(dur)` | `None` | `lossy(false)` 时队列满的最大阻塞时长，超时后降级丢弃 |
| `.shutdown_timeout(dur)` | `30s` | `flush()` 与停机等待队列排空的超时上限（`.no_shutdown_timeout()` 可取消上限） |
| `.console(bool)` | `true` | 是否启用控制台（Stderr）输出 |
| `.file(bool)` | `true` | 是否启用文件输出 |
| `.ansi(bool)` | `true` | 是否对控制台输出启用 ANSI 彩色着色 |
| `.show_target(bool)` | `true` | 是否显示目标模块路径 |
| `.show_thread(bool)` | `false` | 是否显示线程名称 |
| `.show_line_number(bool)` | `false` | 是否显示源码文件名与行号 |
| `.time_format(format)` | `"%Y-%m-%d %H:%M:%S%.3f"` | 时间戳 Chrono 格式化模板 |
| `.utc(bool)` | `false` | 是否统一使用 UTC 时区（同时作用于时间戳与按天/按小时轮转） |
| `.catch_panic(bool)` | `false` | 是否接管进程级 Panic hook 强制抓取堆栈并在退出前同步刷盘 |

环境变量支持：

| 变量 | 说明 |
|:---|:---|
| `OWL_LOG_LEVEL` | `trace` / `debug` / `info` / `warn` / `error` |
| `OWL_LOG_CONSOLE_LEVEL` / `OWL_LOG_FILE_LEVEL` | 独立通道级别（或 `off` 关闭对应通道） |
| `OWL_LOG_ERROR_FILE` | 分级独立文件最低级别（如 `error` / `warn`，或 `off`） |
| `OWL_LOG_FORMAT` | `pretty` / `compact` / `json` |
| `OWL_LOG_CONSOLE_FORMAT` / `OWL_LOG_FILE_FORMAT` | 独立通道输出格式 |
| `OWL_LOG_ROTATION` | 轮转策略：`daily` / `hourly` / `never` / `<N>mb`（如 `50mb`） |
| `OWL_LOG_LANG` | `en` / `zh` |
| `OWL_LOG_CONSOLE` / `OWL_LOG_FILE_ENABLE` / `OWL_LOG_UTC` / `OWL_LOG_PROCESS_ISOLATION` | `true` / `false` / `1` / `0` |
| `OWL_LOG_RETENTION_DAYS` / `OWL_LOG_MAX_FILES` | 非负整数（`0` 表示不限） |
| `OWL_LOG_DIR` / `OWL_LOG_FILE` | 日志保存目录 / 文件名前缀 |
| `OTEL_EXPORTER_OTLP_ENDPOINT` / `OTEL_SERVICE_NAME` | 标准 OpenTelemetry 导出端点与服务名 |
| `RUST_LOG` | `tracing` 完整过滤规则，优先于基础级别配置 |

运行期可观测指标与主动刷盘：

```rust
owl_logger::flush().unwrap();

let dropped = guard.dropped_lines();
let io_errors = guard.sink_errors();
if dropped.total() > 0 || io_errors.total() > 0 {
    eprintln!("dropped={dropped:?}, sink_errors={io_errors:?}");
}
```

---

## 📄 License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
