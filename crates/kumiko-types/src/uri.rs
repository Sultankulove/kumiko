//! Resource identifiers.
//!
//! The core treats a `Uri` as opaque apart from its scheme: `file:` is served by the core
//! file system, any other scheme belongs to whoever registered it (e.g. a plugin resolving
//! read-only content). Only `file:` URIs convert to and from local paths.

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UriError {
    #[error("`{0}` has no valid scheme")]
    MissingScheme(String),
    #[error("`{0}` is not an absolute path")]
    RelativePath(PathBuf),
}

/// An absolute URI such as `file:///home/me/project/Main.txt` or `untitled:1`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Uri(String);

impl Uri {
    pub fn parse(value: impl Into<String>) -> Result<Self, UriError> {
        let value = value.into();
        if scheme_len(&value).is_none() {
            return Err(UriError::MissingScheme(value));
        }
        Ok(Self(value))
    }

    /// Builds a `file://` URI, percent-encoding everything except unreserved characters and `/`.
    pub fn from_file_path(path: impl AsRef<Path>) -> Result<Self, UriError> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(UriError::RelativePath(path.to_path_buf()));
        }
        let mut uri = String::from("file://");
        for &byte in path_bytes(path).iter() {
            if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
                uri.push(char::from(byte));
            } else {
                uri.push_str(&format!("%{byte:02X}"));
            }
        }
        Ok(Self(uri))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The scheme without the trailing `:`, e.g. `file`.
    pub fn scheme(&self) -> &str {
        let len = scheme_len(&self.0).unwrap_or(0);
        &self.0[..len]
    }

    /// Local path for a `file:` URI without authority; `None` for any other URI.
    pub fn to_file_path(&self) -> Option<PathBuf> {
        let encoded = self.0.strip_prefix("file://")?;
        if !encoded.starts_with('/') {
            return None;
        }
        let bytes = percent_decode(encoded)?;
        Some(path_from_bytes(bytes))
    }
}

/// Length of a valid RFC 3986 scheme followed by `:`.
fn scheme_len(value: &str) -> Option<usize> {
    let end = value.find(':')?;
    let scheme = &value[..end];
    let mut chars = scheme.chars();
    let first = chars.next()?;
    let valid = first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then_some(end)
}

fn percent_decode(value: &str) -> Option<Vec<u8>> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = value.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;
    Cow::Borrowed(path.as_os_str().as_bytes())
}

#[cfg(unix)]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

// TODO(platforms): Windows drive letters and UNC paths need their own mapping (spec, section 27).
#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Cow<'_, [u8]> {
    Cow::Owned(path.to_string_lossy().replace('\\', "/").into_bytes())
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

impl TryFrom<String> for Uri {
    type Error = UriError;

    fn try_from(value: String) -> Result<Self, UriError> {
        Self::parse(value)
    }
}

impl std::str::FromStr for Uri {
    type Err = UriError;

    fn from_str(value: &str) -> Result<Self, UriError> {
        Self::parse(value)
    }
}

impl From<Uri> for String {
    fn from(uri: Uri) -> String {
        uri.0
    }
}

impl fmt::Display for Uri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl JsonSchema for Uri {
    fn schema_name() -> Cow<'static, str> {
        "Uri".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string", "format": "uri" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemes() {
        assert_eq!(Uri::parse("file:///tmp/a").unwrap().scheme(), "file");
        assert_eq!(Uri::parse("untitled:1").unwrap().scheme(), "untitled");
        assert_eq!(
            Uri::parse("x-plugin+ro:thing").unwrap().scheme(),
            "x-plugin+ro"
        );
        for bad in ["", "/tmp/a", "1abc:x", ":x", "a b:c"] {
            assert!(Uri::parse(bad).is_err(), "{bad}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_paths_round_trip() {
        let path = Path::new("/home/me/My Project/src/Ünï%code.txt");
        let uri = Uri::from_file_path(path).unwrap();
        assert_eq!(
            uri.as_str(),
            "file:///home/me/My%20Project/src/%C3%9Cn%C3%AF%25code.txt"
        );
        assert_eq!(uri.to_file_path().unwrap(), path);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_round_trip() {
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xFFname"));
        let uri = Uri::from_file_path(path).unwrap();
        assert_eq!(uri.as_str(), "file:///tmp/%FFname");
        assert_eq!(uri.to_file_path().unwrap(), path);
    }

    #[test]
    fn non_file_uris_have_no_path() {
        assert!(Uri::parse("untitled:1").unwrap().to_file_path().is_none());
        assert!(
            Uri::parse("file://host/share")
                .unwrap()
                .to_file_path()
                .is_none()
        );
        assert!(Uri::from_file_path("relative/path").is_err());
    }

    #[test]
    fn serde_validates_uris() {
        let uri: Uri = serde_json::from_str("\"file:///a\"").unwrap();
        assert_eq!(serde_json::to_string(&uri).unwrap(), "\"file:///a\"");
        assert!(serde_json::from_str::<Uri>("\"/a\"").is_err());
    }
}
