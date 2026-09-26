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
    #[error("`{0}` cannot be expressed as a file URI")]
    Unrepresentable(PathBuf),
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

    /// Builds a `file:` URI, percent-encoding everything except unreserved characters, `/`
    /// and a drive letter's `:`. Windows paths map as in RFC 8089: `C:\a` → `file:///C:/a`
    /// (drive letter upper-cased), `\\server\share\a` → `file://server/share/a`; the `\\?\`
    /// prefix of canonical paths is dropped.
    pub fn from_file_path(path: impl AsRef<Path>) -> Result<Self, UriError> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(UriError::RelativePath(path.to_path_buf()));
        }
        let bytes = uri_path(path)?;
        let mut uri = String::from("file:");
        if !bytes.starts_with(b"//") {
            uri.push_str("//");
        }
        for (i, &byte) in bytes.iter().enumerate() {
            let drive_colon = i == 2 && byte == b':' && bytes[1].is_ascii_alphabetic();
            if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) || drive_colon {
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

    /// Local path for a `file:` URI; `None` for any other URI. An authority (a UNC server)
    /// is accepted on Windows only.
    pub fn to_file_path(&self) -> Option<PathBuf> {
        let encoded = self.0.strip_prefix("file://")?;
        path_from_uri_path(percent_decode(encoded)?)
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

/// The URI path of an absolute local path, before percent-encoding: `/home/me` on Unix.
#[cfg(unix)]
fn uri_path(path: &Path) -> Result<Cow<'_, [u8]>, UriError> {
    use std::os::unix::ffi::OsStrExt;
    Ok(Cow::Borrowed(path.as_os_str().as_bytes()))
}

/// The decoded part of a `file://` URI after the scheme: a path starting with `/`.
#[cfg(unix)]
fn path_from_uri_path(bytes: Vec<u8>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    bytes
        .starts_with(b"/")
        .then(|| PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// The URI path of an absolute local path (WASI): `/dir`, which must be Unicode.
#[cfg(not(any(unix, windows)))]
fn uri_path(path: &Path) -> Result<Cow<'_, [u8]>, UriError> {
    let text = path
        .to_str()
        .ok_or_else(|| UriError::Unrepresentable(path.to_path_buf()))?;
    Ok(Cow::Borrowed(text.as_bytes()))
}

#[cfg(not(any(unix, windows)))]
fn path_from_uri_path(bytes: Vec<u8>) -> Option<PathBuf> {
    let text = String::from_utf8(bytes).ok()?;
    text.starts_with('/').then(|| PathBuf::from(text))
}

/// The URI path of an absolute local path, before percent-encoding: `/C:/Users/me` for a
/// drive path, `//server/share/dir` for a UNC path (the server becomes the authority).
#[cfg(windows)]
fn uri_path(path: &Path) -> Result<Cow<'_, [u8]>, UriError> {
    let unrepresentable = || UriError::Unrepresentable(path.to_path_buf());
    let text = path
        .to_str()
        .ok_or_else(unrepresentable)?
        .replace('\\', "/");
    let text = if let Some(unc) = text.strip_prefix("//?/UNC/") {
        format!("//{unc}")
    } else if let Some(local) = text.strip_prefix("//?/") {
        local.to_owned()
    } else {
        text
    };
    if text.starts_with("//") {
        // `//./` and `//?/` name devices, not files.
        return match text.as_bytes().get(2) {
            Some(b'.' | b'?') => Err(unrepresentable()),
            _ => Ok(Cow::Owned(text.into_bytes())),
        };
    }
    match text.as_bytes() {
        [drive, b':', rest @ ..] if drive.is_ascii_alphabetic() => {
            let mut out = vec![b'/', drive.to_ascii_uppercase(), b':'];
            out.extend_from_slice(rest);
            if rest.is_empty() {
                out.push(b'/');
            }
            Ok(Cow::Owned(out))
        }
        _ => Err(unrepresentable()),
    }
}

/// The decoded part of a `file://` URI after the scheme: `/C:/dir` (or `/c%3A/dir`, as
/// VS Code writes it) for a drive path, `server/share/dir` for a UNC path.
#[cfg(windows)]
fn path_from_uri_path(bytes: Vec<u8>) -> Option<PathBuf> {
    let text = String::from_utf8(bytes).ok()?;
    let path = match text.strip_prefix('/') {
        Some(local) => match local.as_bytes() {
            [drive, b':', ..] if drive.is_ascii_alphabetic() => {
                let rest = if local.len() == 2 { "/" } else { &local[2..] };
                format!("{}:{rest}", char::from(drive.to_ascii_uppercase()))
            }
            _ => return None,
        },
        None if text.is_empty() || text.starts_with('/') => return None,
        None => format!("//{text}"),
    };
    Some(PathBuf::from(path.replace('/', "\\")))
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

    #[cfg(windows)]
    #[test]
    fn windows_paths_round_trip() {
        let cases = [
            (
                r"C:\Users\me\My Project\Ünï%code.txt",
                "file:///C:/Users/me/My%20Project/%C3%9Cn%C3%AF%25code.txt",
            ),
            (r"D:\", "file:///D:/"),
            (r"\\server\share\dir\a.txt", "file://server/share/dir/a.txt"),
        ];
        for (path, expected) in cases {
            let uri = Uri::from_file_path(path).unwrap();
            assert_eq!(uri.as_str(), expected);
            assert_eq!(uri.to_file_path().unwrap(), Path::new(path));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_are_normalized() {
        let same = [r"c:\a\b.txt", r"\\?\C:\a\b.txt", r"C:/a/b.txt"];
        for path in same {
            assert_eq!(
                Uri::from_file_path(path).unwrap().as_str(),
                "file:///C:/a/b.txt",
                "{path}"
            );
        }
        assert_eq!(
            Uri::from_file_path(r"\\?\UNC\server\share\a")
                .unwrap()
                .as_str(),
            "file://server/share/a"
        );
        // VS Code encodes the drive colon and lower-cases the letter.
        let vscode = Uri::parse("file:///c%3A/a/b.txt").unwrap();
        assert_eq!(vscode.to_file_path().unwrap(), Path::new(r"C:\a\b.txt"));
        assert!(Uri::from_file_path(r"\\.\pipe\name").is_err());
        assert!(
            Uri::parse("file:///home/me")
                .unwrap()
                .to_file_path()
                .is_none()
        );
    }

    #[test]
    fn non_file_uris_have_no_path() {
        assert!(Uri::parse("untitled:1").unwrap().to_file_path().is_none());
        assert!(Uri::from_file_path("relative/path").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unix_file_uris_have_no_authority() {
        assert!(
            Uri::parse("file://host/share")
                .unwrap()
                .to_file_path()
                .is_none()
        );
    }

    #[test]
    fn serde_validates_uris() {
        let uri: Uri = serde_json::from_str("\"file:///a\"").unwrap();
        assert_eq!(serde_json::to_string(&uri).unwrap(), "\"file:///a\"");
        assert!(serde_json::from_str::<Uri>("\"/a\"").is_err());
    }
}
