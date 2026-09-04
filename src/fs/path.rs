use alloc::string::String;
use alloc::vec::Vec;

use super::error::FsError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path {
    inner: String,
}

impl Path {
    pub fn root() -> Self {
        Self {
            inner: String::from("/"),
        }
    }

    pub fn new_absolute(raw: &str) -> Result<Self, FsError> {
        if !raw.starts_with('/') {
            return Err(FsError::InvalidPath);
        }

        Self::normalize(raw)
    }

    /// Resolve `raw` against `cwd` and return a canonical absolute path.
    ///
    /// This is intentionally lexical for now: there are no symlinks or mount
    /// namespaces yet, so `.` and `..` can safely be normalized before inode
    /// lookup.
    pub fn resolve(cwd: &Self, raw: &str) -> Result<Self, FsError> {
        if raw.is_empty() {
            return Err(FsError::InvalidPath);
        }

        if raw.starts_with('/') {
            return Self::normalize(raw);
        }

        let mut combined = String::new();
        combined.push_str(cwd.as_str());
        if !combined.ends_with('/') {
            combined.push('/');
        }
        combined.push_str(raw);
        Self::normalize(&combined)
    }

    fn normalize(raw: &str) -> Result<Self, FsError> {
        let mut parts: Vec<&str> = Vec::new();

        for part in raw.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                _ => {
                    if part.as_bytes().iter().any(|&b| b == 0) {
                        return Err(FsError::InvalidPath);
                    }
                    parts.push(part);
                }
            }
        }

        let mut normalized = String::from("/");
        for (index, part) in parts.iter().enumerate() {
            if index != 0 {
                normalized.push('/');
            }
            normalized.push_str(part);
        }

        Ok(Self { inner: normalized })
    }

    pub fn as_str(&self) -> &str {
        &self.inner
    }

    pub fn is_root(&self) -> bool {
        self.inner == "/"
    }

    pub fn file_name(&self) -> Option<&str> {
        if self.is_root() {
            return None;
        }

        self.inner.rsplit('/').next()
    }

    pub fn parent(&self) -> Self {
        if self.is_root() {
            return Self::root();
        }

        let slash = self.inner.rfind('/').unwrap_or(0);
        if slash == 0 {
            Self::root()
        } else {
            Self {
                inner: String::from(&self.inner[..slash]),
            }
        }
    }

    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.inner.split('/').filter(|component| !component.is_empty())
    }
}
