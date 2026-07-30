use std::error::Error;
use std::fmt;

pub type VfsResult<T> = Result<T, VfsError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VfsError {
    code: &'static str,
    message: String,
}

impl VfsError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn enoent(path: impl AsRef<str>) -> Self {
        Self::new(
            "ENOENT",
            format!("no such file or directory: {}", path.as_ref()),
        )
    }

    pub fn eexist(path: impl AsRef<str>) -> Self {
        Self::new("EEXIST", format!("file exists: {}", path.as_ref()))
    }

    pub fn enotdir(path: impl AsRef<str>) -> Self {
        Self::new("ENOTDIR", format!("not a directory: {}", path.as_ref()))
    }

    pub fn eisdir(path: impl AsRef<str>) -> Self {
        Self::new("EISDIR", format!("is a directory: {}", path.as_ref()))
    }

    pub fn eloop(path: impl AsRef<str>) -> Self {
        Self::new(
            "ELOOP",
            format!("too many symbolic links: {}", path.as_ref()),
        )
    }

    pub fn enametoolong(path: impl AsRef<str>) -> Self {
        Self::new("ENAMETOOLONG", format!("path too long: {}", path.as_ref()))
    }

    pub fn enotempty(path: impl AsRef<str>) -> Self {
        Self::new(
            "ENOTEMPTY",
            format!("directory not empty: {}", path.as_ref()),
        )
    }

    pub fn eopnotsupp(message: impl Into<String>) -> Self {
        Self::new("EOPNOTSUPP", message)
    }

    pub fn erofs(message: impl Into<String>) -> Self {
        Self::new("EROFS", message)
    }

    pub fn einval(message: impl Into<String>) -> Self {
        Self::new("EINVAL", message)
    }

    pub fn eio(message: impl Into<String>) -> Self {
        Self::new("EIO", message)
    }
}

impl fmt::Display for VfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for VfsError {}
