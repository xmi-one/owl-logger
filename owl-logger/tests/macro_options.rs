//! 集成测试：monitor 的省略选项不能给函数附加 Debug trait 约束。

use owl_logger::monitor;

struct NotDebug;

#[monitor(skip_all, skip_return)]
fn accepts_non_debug_input_and_output(value: NotDebug) -> NotDebug {
    value
}

#[test]
fn skip_options_keep_monitor_compatible_with_non_debug_types() {
    let _ = accepts_non_debug_input_and_output(NotDebug);
}
