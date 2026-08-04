//! Stock-client `Data` path resolution shared by the CLI and desktop GUI.
//!
//! The Rust server reads `kart.rho`, `item.rho`, and RHO5 overlays directly.
//! Selecting a client root or its `Profile` directory therefore never requires
//! the C# server's generated `KartCatalog.xml` sidecar.

use std::{
    ffi::OsStr,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, anyhow};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientRuntimePaths {
    pub(crate) client_data_dir: Option<PathBuf>,
}

/// Resolves an optional stock-client installation, `Profile` directory,
/// `Data` directory, or legacy catalog path into the authoritative `Data`
/// directory used by the Rust RHO readers.
///
/// An explicit data-directory override always wins. A legacy
/// `Profile/KartCatalog.xml` selection is accepted only to locate its sibling
/// `Data` directory; the XML itself is not opened.
pub(crate) fn resolve_client_runtime_paths(
    client_path: Option<&Path>,
    explicit_data_dir: Option<&Path>,
) -> Result<ClientRuntimePaths> {
    let data_dir = explicit_data_dir
        .map(Path::to_owned)
        .or_else(|| client_path.map(infer_data_directory));
    let client_data_dir = data_dir
        .as_deref()
        .map(require_data_directory)
        .transpose()?;
    Ok(ClientRuntimePaths { client_data_dir })
}

fn require_data_directory(path: &Path) -> Result<PathBuf> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::canonicalize(path).with_context(|| {
            format!(
                "클라이언트 Data 실제 경로를 확인하지 못했습니다: {}",
                path.display()
            )
        }),
        Ok(_) => Err(anyhow!(
            "클라이언트 Data 경로가 디렉터리가 아닙니다: {}",
            path.display()
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Err(anyhow!(
            "클라이언트 Data 디렉터리를 찾지 못했습니다: {}",
            path.display()
        )),
        Err(error) => Err(error).with_context(|| {
            format!(
                "클라이언트 Data 경로를 확인하지 못했습니다: {}",
                path.display()
            )
        }),
    }
}

fn infer_data_directory(input: &Path) -> PathBuf {
    if file_name_is(input, "Data") {
        return input.to_owned();
    }
    if file_name_is(input, "Profile") {
        return input
            .parent()
            .map_or_else(|| input.join("Data"), |root| root.join("Data"));
    }
    if has_extension(input, "xml") {
        return input
            .parent()
            .filter(|parent| file_name_is(parent, "Profile"))
            .and_then(Path::parent)
            .map_or_else(|| input.with_file_name("Data"), |root| root.join("Data"));
    }
    input.join("Data")
}

fn has_extension(path: &Path, expected: &str) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

fn file_name_is(path: &Path, expected: &str) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::resolve_client_runtime_paths;

    #[test]
    fn client_root_resolves_data_without_a_catalog_xml() {
        let root = tempdir().unwrap();
        let data = root.path().join("Data");
        fs::create_dir(&data).unwrap();

        let paths = resolve_client_runtime_paths(Some(root.path()), None).unwrap();

        assert_eq!(
            paths.client_data_dir.as_deref(),
            Some(fs::canonicalize(data).unwrap().as_path())
        );
    }

    #[test]
    fn profile_directory_resolves_the_sibling_data_directory() {
        let root = tempdir().unwrap();
        let profile = root.path().join("Profile");
        let data = root.path().join("Data");
        fs::create_dir(&profile).unwrap();
        fs::create_dir(&data).unwrap();

        let paths = resolve_client_runtime_paths(Some(&profile), None).unwrap();

        assert_eq!(paths.client_data_dir, Some(fs::canonicalize(data).unwrap()));
    }

    #[test]
    fn data_directory_can_be_selected_directly() {
        let root = tempdir().unwrap();
        let data = root.path().join("Data");
        fs::create_dir(&data).unwrap();

        let paths = resolve_client_runtime_paths(Some(&data), None).unwrap();

        assert_eq!(paths.client_data_dir, Some(fs::canonicalize(data).unwrap()));
    }

    #[test]
    fn legacy_xml_selection_only_infers_the_sibling_data_directory() {
        let root = tempdir().unwrap();
        let profile = root.path().join("Profile");
        let data = root.path().join("Data");
        fs::create_dir(&profile).unwrap();
        fs::create_dir(&data).unwrap();

        let catalog = profile.join("KartCatalog.xml");
        let paths = resolve_client_runtime_paths(Some(&catalog), None).unwrap();

        assert_eq!(paths.client_data_dir, Some(fs::canonicalize(data).unwrap()));
    }

    #[test]
    fn explicit_data_override_wins_without_inspecting_the_client_path() {
        let root = tempdir().unwrap();
        let override_data = root.path().join("alternate-data");
        fs::create_dir(&override_data).unwrap();

        let paths = resolve_client_runtime_paths(
            Some(&root.path().join("missing-client")),
            Some(&override_data),
        )
        .unwrap();

        assert_eq!(
            paths.client_data_dir,
            Some(fs::canonicalize(override_data).unwrap())
        );
    }

    #[test]
    fn missing_data_directory_reports_the_inferred_path() {
        let root = tempdir().unwrap();

        let error = resolve_client_runtime_paths(Some(root.path()), None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("Data"));
        assert!(!error.contains("KartCatalog.xml"));
    }
}
