# 🦉 owl-logger

**开箱即用、生产级的 Rust 日志库** — 基于 `tracing` 生态构建。

[![Crates.io](https://img.shields.io/crates/v/owl-logger.svg)](https://crates.io/crates/owl-logger)
[![Docs.rs](https://docs.rs/owl-logger/badge.svg)](https://docs.rs/owl-logger)
[![License](https://img.shields.io/crates/l/owl-logger.svg)](LICENSE)

## ✨ 特性

- 🚀 **一行初始化** — 零配置即可开始使用
- 🎨 **彩色输出** — 控制台日志带有颜色高亮
- 📁 **文件轮转** — 支持按天、按小时自动轮转
- 🌏 **多语言** — 支持中文/英文系统提示语（日志级别统一使用英文）
- 🔗 **上下文追踪** — 自动注入 `request_id`，支持同步/异步
- ⏱️ **函数监控** — `#[monitor]` 宏自动记录入参、返回值和耗时
- 🧹 **自动清理** — `Drop` trait 自动 flush，无需手动 cleanup
- 📊 **JSON 输出** — 生产环境结构化日志
- 🔄 **环境过滤** — 支持 `RUST_LOG` 环境变量动态控制
- 🔌 **log 兼容** — 自动桥接 `log` crate 生态

## 📦 安装

```toml
[dependencies]
owl-logger = "0.1"
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
        .level(LogLevel::Debug)
        .rotation(RotationPolicy::Daily)
        .show_line_number(true)
        .init();

    owl_logger::info!("🦉 开始工作！");
}
```

## 📖 功能详解

### 请求上下文追踪

类似 Python 的 `contextvars`，在 Span 作用域内自动注入 `req_id`：

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

### 函数监控宏

类似 Python 的 `@log_decorator()`，自动记录函数执行信息：

```rust
#[owl_logger::monitor]
fn process_order(order_id: &str, amount: f64) -> bool {
    // 自动输出：
    // → entering process_order(order_id="ORD-001", amount=99.9)
    // ← exiting process_order — elapsed 12.3ms — returned true
    true
}

// 跳过敏感参数
#[owl_logger::monitor(level = "debug", skip(password))]
fn login(username: &str, password: &str) -> bool {
    true
}
```

### 输出示例

**中文模式 (`Language::Zh`)**：
```
2025-05-30 10:30:15 | INFO  | request{req_id=req-001} | my_app > 订单创建成功
2025-05-30 10:30:15 | WARN  | request{req_id=req-001} | my_app > 库存不足
```

**英文模式 (`Language::En`)**：
```
2025-05-30 10:30:15 | INFO  | request{req_id=req-001} | my_app > Order created
2025-05-30 10:30:15 | WARN  | request{req_id=req-001} | my_app > Insufficient stock
```

## ⚙️ 配置项

| 方法 | 默认值 | 说明 |
|:---|:---|:---|
| `.file_name(name)` | `"app"` | 日志文件名前缀 |
| `.log_dir(dir)` | `"logs"` | 日志目录 |
| `.level(level)` | `Info` | 最低日志级别 |
| `.language(lang)` | `En` | 输出语言（En / Zh） |
| `.format(fmt)` | `Pretty` | 输出格式（Pretty / Compact / Json） |
| `.rotation(policy)` | `Daily` | 文件轮转（Daily / Hourly / Never） |
| `.console(bool)` | `true` | 启用控制台输出 |
| `.file(bool)` | `true` | 启用文件输出 |
| `.ansi(bool)` | `true` | 启用 ANSI 彩色 |
| `.show_target(bool)` | `true` | 显示来源模块 |
| `.show_thread(bool)` | `false` | 显示线程名 |
| `.show_line_number(bool)` | `false` | 显示行号 |

## 🔧 环境变量

支持 `RUST_LOG` 环境变量覆盖配置的日志级别：

```bash
# 全局 debug
RUST_LOG=debug cargo run

# 仅 my_app 模块开启 trace
RUST_LOG=warn,my_app=trace cargo run
```

## 📐 架构

```
owl-logger (workspace)
├── owl-logger/          # 主库 crate
│   └── src/
│       ├── lib.rs       # 公开 API
│       ├── builder.rs   # Builder 模式
│       ├── config.rs    # 配置类型
│       ├── guard.rs     # Guard + Drop
│       ├── context.rs   # 上下文追踪
│       ├── formatter.rs # 自定义格式化
│       ├── i18n.rs      # 多语言
│       └── error.rs     # 错误类型
├── owl-logger-macros/   # 过程宏 crate
│   └── src/lib.rs       # #[monitor] 宏
└── examples/            # 使用示例
```

## 📄 License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
