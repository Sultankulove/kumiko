//! Files GPUI loads by path: the SVG icons of icon packs (spec, section 21). Paths are the
//! absolute file paths the Resources service gives, so no asset is compiled in.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub(crate) struct Files;

impl AssetSource for Files {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(Cow::Owned(bytes))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}
