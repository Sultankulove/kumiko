//! Loading a grammar package (spec, section 18): a native parser library found through the
//! package manifest, checked against the Tree-sitter ABI this build speaks, plus queries.

#![allow(unsafe_code)]

use std::path::{Path, PathBuf};

use libloading::Library;
use tree_sitter::{Language, Query};
use tree_sitter_language::LanguageFn;

/// The `[grammar]` table of a package's `resource.toml`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrammarManifest {
    pub language: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Library file name without platform prefix and extension.
    pub library: String,
    pub symbol: String,
}

/// A loaded grammar: the language, its queries and the library that keeps them valid.
pub struct Grammar {
    pub manifest: GrammarManifest,
    pub language: Language,
    pub highlights: Option<Query>,
    pub folds: Option<Query>,
    // Dropped last: the language points into it.
    _library: Library,
}

/// Where the library may be: the package directory, then next to the executable (a
/// development build puts it there).
fn library_paths(dir: &Path, name: &str) -> Vec<PathBuf> {
    let file = libloading::library_filename(name);
    let mut paths = vec![dir.join(&file)];
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        paths.push(exe_dir.join(&file));
        // Test binaries live one level deeper (target/debug/deps).
        if let Some(parent) = exe_dir.parent() {
            paths.push(parent.join(&file));
        }
    }
    paths
}

impl Grammar {
    /// Loads the package in `dir` described by `manifest`.
    pub fn load(dir: &Path, manifest: GrammarManifest) -> Result<Self, String> {
        let path = library_paths(dir, &manifest.library)
            .into_iter()
            .find(|p| p.is_file())
            .ok_or_else(|| format!("parser library `{}` not found", manifest.library))?;
        // SAFETY: loading runs the library's initializers; grammar packages are installed by
        // the user like plugins, and the library only defines the parser tables.
        let library =
            unsafe { Library::new(&path) }.map_err(|e| format!("{}: {e}", path.display()))?;
        let constructor = {
            // SAFETY: the manifest names a function of this type (`TSLanguage *(void)`); the
            // ABI version is checked below before the language is used.
            let symbol = unsafe {
                library.get::<unsafe extern "C" fn() -> *const ()>(manifest.symbol.as_bytes())
            }
            .map_err(|e| format!("{}: {e}", manifest.symbol))?;
            *symbol
        };
        // SAFETY: `constructor` comes from the loaded library, which `Grammar` keeps alive.
        let language = Language::new(unsafe { LanguageFn::from_raw(constructor) });
        let abi = language.abi_version();
        if !(tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION..=tree_sitter::LANGUAGE_VERSION)
            .contains(&abi)
        {
            return Err(format!(
                "parser ABI {abi} is outside {}..={}",
                tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION,
                tree_sitter::LANGUAGE_VERSION
            ));
        }
        let query = |name: &str| -> Result<Option<Query>, String> {
            let file = dir.join("queries").join(name);
            match std::fs::read_to_string(&file) {
                Ok(source) => Query::new(&language, &source)
                    .map(Some)
                    .map_err(|e| format!("{}: {e}", file.display())),
                Err(_) => Ok(None),
            }
        };
        Ok(Self {
            highlights: query("highlights.scm")?,
            folds: query("folds.scm")?,
            manifest,
            language,
            _library: library,
        })
    }
}
