use std::fmt;

/// owl-logger 错误类型
#[derive(Debug)]
pub enum OwlError {
    /// 全局 subscriber 已经被设置
    AlreadyInitialized,
    /// 日志目录创建失败
    LogDirCreation(std::io::Error),
    /// 日志文件 appender 创建失败
    FileAppenderCreation(String),
    /// 环境过滤器解析失败
    EnvFilter(String),
    /// 动态修改日志过滤器/级别失败
    Reload(String),
    /// 日志系统未初始化
    NotInitialized,
    /// 其他错误
    Other(String),
}

impl fmt::Display for OwlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OwlError::AlreadyInitialized => {
                write!(f, "owl-logger: global subscriber already initialized")
            }
            OwlError::LogDirCreation(e) => {
                write!(f, "owl-logger: failed to create log directory: {e}")
            }
            OwlError::FileAppenderCreation(msg) => {
                write!(f, "owl-logger: failed to create file appender: {msg}")
            }
            OwlError::EnvFilter(e) => {
                write!(f, "owl-logger: invalid env filter: {e}")
            }
            OwlError::Reload(e) => {
                write!(f, "owl-logger: failed to reload log filter: {e}")
            }
            OwlError::NotInitialized => {
                write!(f, "owl-logger: logger not initialized")
            }
            OwlError::Other(msg) => {
                write!(f, "owl-logger: {msg}")
            }
        }
    }
}

impl std::error::Error for OwlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            OwlError::LogDirCreation(e) => Some(e),
            _ => None,
        }
    }
}
