//! JDK detection (spec, section 18): `JAVA_HOME`, `PATH`, the usual install folders of each
//! OS, SDKMAN, and folders from the settings. The version comes from the JDK's `release` file.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jdk {
    pub home: PathBuf,
    /// `JAVA_VERSION` of the `release` file, e.g. `21.0.9` or `1.8.0_412`.
    pub version: String,
    /// Feature release: 8 for `1.8.0_412`, 21 for `21.0.9`.
    pub major: u32,
}

impl Jdk {
    /// The execution environment name JDT LS uses (`JavaSE-21`, `JavaSE-1.8`).
    pub fn environment(&self) -> String {
        if self.major <= 8 {
            format!("JavaSE-1.{}", self.major)
        } else {
            format!("JavaSE-{}", self.major)
        }
    }

    pub fn java(&self) -> PathBuf {
        self.home
            .join("bin")
            .join(format!("java{}", std::env::consts::EXE_SUFFIX))
    }
}

/// `21.0.9` → 21, `1.8.0_412` → 8, `17` → 17.
pub fn major(version: &str) -> Option<u32> {
    let mut parts = version.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

/// A JDK at `home`: it has a `release` file with `JAVA_VERSION` and a `javac`.
pub fn read(home: &Path) -> Option<Jdk> {
    let release = std::fs::read_to_string(home.join("release")).ok()?;
    let version = release.lines().find_map(|line| {
        let value = line.strip_prefix("JAVA_VERSION=")?;
        Some(value.trim().trim_matches('"').to_owned())
    })?;
    let javac = home
        .join("bin")
        .join(format!("javac{}", std::env::consts::EXE_SUFFIX));
    if !javac.is_file() {
        return None; // a JRE: projects need a compiler
    }
    Some(Jdk {
        home: home.to_path_buf(),
        major: major(&version)?,
        version,
    })
}

/// Where to look; plain data so tests can build it.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    pub java_home: Option<PathBuf>,
    /// Entries of `PATH`.
    pub path: Vec<PathBuf>,
    /// Folders whose children are JDKs (`/usr/lib/jvm`, `C:\Program Files\Java`, …).
    pub roots: Vec<PathBuf>,
    /// Folders from the settings (`java.jdks`).
    pub configured: Vec<PathBuf>,
}

impl Sources {
    /// The places of this machine.
    pub fn from_env(configured: Vec<PathBuf>) -> Self {
        let home =
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
        let mut roots = Vec::new();
        if let Some(home) = &home {
            roots.push(home.join(".sdkman/candidates/java"));
            roots.push(home.join(".jdks")); // IntelliJ's downloads
            roots.push(home.join("scoop/apps"));
        }
        if cfg!(windows) {
            for var in ["ProgramFiles", "ProgramW6432"] {
                if let Some(base) = std::env::var_os(var).map(PathBuf::from) {
                    for vendor in [
                        "Java",
                        "Eclipse Adoptium",
                        "Microsoft",
                        "Zulu",
                        "Amazon Corretto",
                        "BellSoft",
                        "OpenJDK",
                    ] {
                        roots.push(base.join(vendor));
                    }
                }
            }
        } else {
            roots.push(PathBuf::from("/usr/lib/jvm"));
            roots.push(PathBuf::from("/usr/java"));
            roots.push(PathBuf::from("/opt/java"));
            roots.push(PathBuf::from("/Library/Java/JavaVirtualMachines"));
        }
        Self {
            java_home: std::env::var_os("JAVA_HOME").map(PathBuf::from),
            path: std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).collect())
                .unwrap_or_default(),
            roots,
            configured,
        }
    }
}

/// Folders that may be a JDK home: a root's children, `Contents/Home` on macOS, `current` of
/// scoop apps, and the parent of a `bin` with `java` on `PATH`.
fn candidates(sources: &Sources) -> Vec<PathBuf> {
    let mut found = Vec::new();
    found.extend(sources.configured.iter().cloned());
    found.extend(sources.java_home.iter().cloned());
    let java = format!("java{}", std::env::consts::EXE_SUFFIX);
    for dir in &sources.path {
        let exe = dir.join(&java);
        if exe.is_file() {
            // Through symlinks (/usr/bin/java → /usr/lib/jvm/…/bin/java).
            let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
            if let Some(home) = exe.parent().and_then(Path::parent) {
                found.push(home.to_path_buf());
            }
        }
    }
    for root in &sources.roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            found.push(dir.join("Contents/Home"));
            found.push(dir.join("current"));
            found.push(dir);
        }
    }
    found
}

/// Every JDK found, newest first; each home once.
pub fn detect(sources: &Sources) -> Vec<Jdk> {
    let mut seen = BTreeSet::new();
    let mut jdks: Vec<Jdk> = candidates(sources)
        .into_iter()
        .filter_map(|dir| read(&dir))
        .filter(|jdk| {
            let key = std::fs::canonicalize(&jdk.home).unwrap_or_else(|_| jdk.home.clone());
            seen.insert(key)
        })
        .collect();
    jdks.sort_by(|a, b| {
        b.major
            .cmp(&a.major)
            .then_with(|| b.version.cmp(&a.version))
    });
    jdks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_jdk(dir: &Path, version: &str) -> PathBuf {
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(
            dir.join("release"),
            format!("JAVA_VERSION=\"{version}\"\nOS_NAME=x\n"),
        )
        .unwrap();
        for tool in ["java", "javac"] {
            std::fs::write(
                dir.join("bin")
                    .join(format!("{tool}{}", std::env::consts::EXE_SUFFIX)),
                "",
            )
            .unwrap();
        }
        dir.to_path_buf()
    }

    #[test]
    fn versions() {
        assert_eq!(major("21.0.9"), Some(21));
        assert_eq!(major("1.8.0_412"), Some(8));
        assert_eq!(major("17"), Some(17));
        assert_eq!(major("25-ea"), Some(25));
        assert_eq!(major("x"), None);
    }

    #[test]
    fn detects_from_every_source_once_newest_first() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("jvm");
        let jdk17 = fake_jdk(&root.join("jdk-17"), "17.0.12");
        let jdk21 = fake_jdk(&root.join("temurin-21"), "21.0.9");
        let jdk8 = fake_jdk(&temp.path().join("old"), "1.8.0_412");
        // A JRE (no javac) is not offered.
        let jre = temp.path().join("jre");
        std::fs::create_dir_all(jre.join("bin")).unwrap();
        std::fs::write(jre.join("release"), "JAVA_VERSION=\"11.0.1\"\n").unwrap();
        let sources = Sources {
            java_home: Some(jdk17.clone()),
            path: vec![jdk21.join("bin"), jre.join("bin")],
            roots: vec![root, temp.path().join("missing")],
            configured: vec![jdk8.clone(), jre],
        };
        let found = detect(&sources);
        let majors: Vec<u32> = found.iter().map(|j| j.major).collect();
        assert_eq!(majors, [21, 17, 8]);
        assert_eq!(found[2].environment(), "JavaSE-1.8");
        assert_eq!(found[0].environment(), "JavaSE-21");
    }
}
