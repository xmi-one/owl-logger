//! 编译期夹具：验证 `owl-logger` 被依赖方重命名后，`#[monitor]` 仍能正确展开。

use owl_logger_alias::monitor;

pub struct NotDebug;

#[monitor(skip_all, skip_return)]
pub fn works_with_a_renamed_dependency(value: NotDebug) -> NotDebug {
    value
}

#[cfg(test)]
mod tests {
    use super::{works_with_a_renamed_dependency, NotDebug};

    #[test]
    fn monitor_macro_uses_the_dependency_alias() {
        let _ = works_with_a_renamed_dependency(NotDebug);
    }
}
