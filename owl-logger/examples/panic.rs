//! Panic 捕获与崩溃日志记录示例
//!
//! 运行：`cargo run --example panic -p owl-logger`

fn main() {
    let _guard = owl_logger::builder()
        .file_name("panic_example")
        .log_dir("logs")
        .catch_panic(true) // 显式开启捕获崩溃（默认即为 true）
        .init();

    owl_logger::info!("程序正常启动，准备触发一个 panic 崩溃...");

    // 触发 panic 崩溃
    panic!("这是一个测试 panic，用于验证崩溃日志捕获与堆栈输出！");
}
