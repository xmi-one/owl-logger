//! 集成测试：验证日志清理的隔离性与保护机制。
//! 确保：
//! 1. 活跃的日志文件（如 app.log, app.error.log）即便已过期也绝对不被清理。
//! 2. 属于当前组件的过期备份/轮转文件（如 app.1.log）被正常清理。
//! 3. 属于其他组件（如 app.helper）的日志文件互不干扰，不被清理。

use owl_logger::RotationPolicy;
use std::path::{Path, PathBuf};

fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("owl-test-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// 辅助函数：将文件的修改时间设置为很久以前（2020-01-01），模拟过期文件
fn set_expired_time(path: &Path) {
    let status = std::process::Command::new("touch")
        .args(&["-t", "202001010000", path.to_str().unwrap()])
        .status();
    assert!(
        status.map(|s| s.success()).unwrap_or(false),
        "Failed to set expired time for {:?}",
        path
    );
}

#[test]
fn test_cleanup_isolation_and_active_protection() {
    let dir = unique_temp_dir("cleanup_protection");

    // 1. 在测试目录中准备各种假文件
    let active_log = dir.join("app.log");
    let active_error_log = dir.join("app.error.log");
    let expired_rolled_log = dir.join("app.1.log");
    let expired_date_log = dir.join("app.log.2020-01-01");
    let expired_error_rolled_log = dir.join("app.error.1.log");

    // 其他组件的日志文件（如 app.helper）
    let helper_active_log = dir.join("app.helper.log");
    let helper_rolled_log = dir.join("app.helper.1.log");
    let helper_date_log = dir.join("app.helper.log.2020-01-01");

    // 写入初始空内容并设置修改时间为 2020 年（早已过期）
    for file in &[
        &active_log,
        &active_error_log,
        &expired_rolled_log,
        &expired_date_log,
        &expired_error_rolled_log,
        &helper_active_log,
        &helper_rolled_log,
        &helper_date_log,
    ] {
        std::fs::write(file, "").unwrap();
        set_expired_time(file);
    }

    // 2. 初始化 "app" 日志组件，设置保留天数为 7 天（这会触发 cleanup_old_logs）
    {
        let _guard = owl_logger::builder()
            .log_dir(dir.to_string_lossy().to_string())
            .file_name("app")
            .console(false)
            .retention_days(7)
            .rotation(RotationPolicy::Never)
            .catch_panic(false)
            .init();
    } // drop guard -> flush

    // 3. 验证清理结果

    // A. 活跃文件保护：正在写入的活跃日志文件即便过期，也绝对不应该被清理
    assert!(active_log.exists(), "Active log file app.log must be protected from deletion");
    assert!(active_error_log.exists(), "Active error log file app.error.log must be protected from deletion");

    // B. 清理过期文件：属于当前组件的、已过期的轮转/备份日志，必须被成功删除
    assert!(!expired_rolled_log.exists(), "Expired rolled log app.1.log should be cleaned up");
    assert!(!expired_date_log.exists(), "Expired date log app.log.2020-01-01 should be cleaned up");
    assert!(!expired_error_rolled_log.exists(), "Expired error rolled log app.error.1.log should be cleaned up");

    // C. 隔离性保护：属于其他组件（app.helper）的日志文件（无论活跃与否、过期与否），绝不能被误删
    assert!(helper_active_log.exists(), "Other app's active log app.helper.log must not be deleted");
    assert!(helper_rolled_log.exists(), "Other app's rolled log app.helper.1.log must not be deleted");
    assert!(helper_date_log.exists(), "Other app's date log app.helper.log.2020-01-01 must not be deleted");

    let _ = std::fs::remove_dir_all(&dir);
}
