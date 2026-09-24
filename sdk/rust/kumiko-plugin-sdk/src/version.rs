//! Protocol version compatibility, shared by process and WASM plugins.

/// Same major version; in 0.x the minor version must match too (semver caret rules).
#[cfg_attr(not(any(feature = "process", feature = "wasm")), allow(dead_code))]
pub(crate) fn compatible(host: &str, plugin: &str) -> bool {
    let parts = |v: &str| {
        let mut numbers = v.split('.').map(|n| n.parse::<u64>().ok());
        (numbers.next().flatten(), numbers.next().flatten())
    };
    match (parts(host), parts(plugin)) {
        ((Some(0), Some(a)), (Some(0), Some(b))) => a == b,
        ((Some(a), Some(_)), (Some(b), Some(_))) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::compatible;

    #[test]
    fn version_compatibility() {
        assert!(compatible("0.1.0", "0.1.7"));
        assert!(!compatible("0.2.0", "0.1.0"));
        assert!(compatible("1.4.0", "1.0.0"));
        assert!(!compatible("2.0.0", "1.9.0"));
        assert!(!compatible("one", "0.1.0"));
    }
}
