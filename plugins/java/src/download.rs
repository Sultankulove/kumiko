//! JDT LS is not bundled (spec, section 18): the first start downloads a pinned milestone
//! into the plugin's cache, checks its SHA-256 and unpacks it to `jdtls/<version>/`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::jdtls;

pub const VERSION: &str = "1.61.0";
const FILE: &str = "jdt-language-server-1.61.0-202609031315.tar.gz";
const SHA256: &str = "338e7e73d61836651ba2453919a0d34fa763eb4e7c03342092309bffb8934c64";
const BASE: &str = "https://download.eclipse.org/jdtls/milestones/1.61.0/";

/// One download at a time (several projects may ask at once).
static DOWNLOADING: Mutex<()> = Mutex::new(());

/// The pinned JDT LS in `cache`, downloaded first if it is not there.
pub fn install(cache: &Path) -> Result<jdtls::Install, String> {
    let _one = DOWNLOADING.lock().unwrap_or_else(|e| e.into_inner());
    let target = cache.join("jdtls").join(VERSION);
    if let Some(install) = jdtls::find(&target) {
        return Ok(install);
    }
    std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let archive = cache.join(format!("{FILE}.part"));
    eprintln!("kumiko-java: downloading JDT LS {VERSION} from {BASE}");
    fetch(&format!("{BASE}{FILE}"), &archive)?;
    let result = verify(&archive, SHA256).and_then(|()| unpack(&archive, &target));
    let _ = std::fs::remove_file(&archive);
    result?;
    jdtls::find(&target).ok_or_else(|| format!("the JDT LS archive has no launcher: {FILE}"))
}

fn fetch(url: &str, to: &Path) -> Result<(), String> {
    let failed = |e: &dyn std::fmt::Display| format!("cannot download {url}: {e}");
    let mut response = ureq::get(url).call().map_err(|e| failed(&e))?;
    let mut body = response.body_mut().as_reader();
    let mut file = std::fs::File::create(to).map_err(|e| failed(&e))?;
    std::io::copy(&mut body, &mut file).map_err(|e| failed(&e))?;
    file.flush().map_err(|e| failed(&e))
}

/// The file's SHA-256 is `expected` (hex).
pub fn verify(path: &Path, expected: &str) -> Result<(), String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!(
            "the download is damaged or not the expected one (SHA-256 {actual}, expected {expected})"
        ))
    }
}

/// Unpacks a `.tar.gz` next to `target` and moves it into place, so a half-unpacked server
/// is never found.
pub fn unpack(archive: &Path, target: &Path) -> Result<(), String> {
    let staging: PathBuf = target.with_extension("unpacking");
    let _ = std::fs::remove_dir_all(&staging);
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    tar.unpack(&staging)
        .map_err(|e| format!("cannot unpack {}: {e}", archive.display()))?;
    let _ = std::fs::remove_dir_all(target);
    std::fs::rename(&staging, target).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tar.gz with a launcher and a configuration, as JDT LS archives have.
    fn archive(dir: &Path) -> PathBuf {
        let path = dir.join("server.tar.gz");
        let file = std::fs::File::create(&path).unwrap();
        let gz = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        for name in [
            "plugins/org.eclipse.equinox.launcher_1.7.0.v1.jar",
            "config_win/config.ini",
            "config_linux/config.ini",
            "config_linux_arm/config.ini",
            "config_mac/config.ini",
            "config_mac_arm/config.ini",
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(1);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, &b"x"[..]).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
        path
    }

    fn sha256(path: &Path) -> String {
        let data = std::fs::read(path).unwrap();
        Sha256::digest(&data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[test]
    fn checks_the_checksum_and_unpacks_into_place() {
        let temp = tempfile::tempdir().unwrap();
        let archive = archive(temp.path());
        assert!(
            verify(&archive, &"0".repeat(64)).is_err(),
            "a wrong sum is refused"
        );
        verify(&archive, &sha256(&archive)).unwrap();
        let target = temp.path().join("jdtls").join(VERSION);
        unpack(&archive, &target).unwrap();
        let install = jdtls::find(&target).unwrap();
        assert!(
            install
                .launcher
                .ends_with("org.eclipse.equinox.launcher_1.7.0.v1.jar")
        );
        assert!(!target.with_extension("unpacking").exists());
    }

    /// The real download of the pinned milestone (about 50 MB).
    #[test]
    #[ignore = "downloads JDT LS: run with KUMIKO_NETWORK=1 and --ignored"]
    fn downloads_the_pinned_milestone() {
        if std::env::var_os("KUMIKO_NETWORK").is_none() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let install = install(temp.path()).unwrap();
        assert!(install.launcher.is_file());
    }

    #[test]
    fn a_present_install_is_not_downloaded_again() {
        let temp = tempfile::tempdir().unwrap();
        let archive = archive(temp.path());
        unpack(&archive, &temp.path().join("jdtls").join(VERSION)).unwrap();
        // No network here: `install` must find it.
        assert!(install(temp.path()).is_ok());
    }
}
