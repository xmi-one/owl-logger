use std::fmt;

/// owl-logger 错误类型
#[derive(Debug)]
pub enum OwlError {
    /// 全局 subscriber 已经被设置
    AlreadyInitialized,
    /// 日志目录创建失败
    LogDirCreation(std::io::Error),
    /// 环境过滤器解析失败
    EnvFilter(String),
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
            OwlError::EnvFilter(e) => {
                write!(f, "owl-logger: invalid env filter: {e}")
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
