//! Версия приложения.
//!
//! Формат — SemVer плюс номер сборки: `0.1.0+build.42`.
//! Версия берётся из `Cargo.toml`, номер сборки — из `build.rs`
//! (переменная окружения `SWITCHER_BUILD_NUMBER`).

/// Версия с номером сборки: `0.1.0+build.42`.
pub fn full() -> String {
    format!(
        "{}+build.{}",
        env!("CARGO_PKG_VERSION"),
        env!("SWITCHER_BUILD_NUMBER")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_has_semver_and_build_metadata() {
        let version = full();
        assert!(version.contains('+'), "missing build metadata: {version}");
        assert!(
            version.contains("+build."),
            "unexpected build metadata: {version}"
        );
        assert!(!version.contains(' '), "spaces are not allowed: {version}");

        let (base, build) = version.split_once('+').expect("split failed");
        assert_eq!(base.split('.').count(), 3, "not SemVer: {version}");
        assert!(
            build.starts_with("build."),
            "unexpected build part: {build}"
        );
        assert!(
            build["build.".len()..].chars().all(|c| c.is_ascii_digit()),
            "build number is not numeric: {version}"
        );
    }
}
