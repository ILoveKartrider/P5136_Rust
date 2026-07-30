//! Stock-client data path resolution shared by the CLI and desktop GUI.
//!
//! The legacy C# server creates `Profile/KartCatalog.xml` beside the stock
//! client. Users should be able to select that installation or its `Profile`
//! directory, rather than having to select the XML file itself.

use std::{
    ffi::OsStr,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, anyhow};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientRuntimePaths {
    pub(crate) catalog_path: Option<PathBuf>,
    pub(crate) client_data_dir: Option<PathBuf>,
}

/// Resolves an optional stock-client installation, `Profile` directory, or
/// catalog XML file into the paths needed by the Rust server.
///
/// An explicit data directory always wins. Otherwise, an existing sibling
/// `Data` directory is inferred from the C# `Profile/KartCatalog.xml` layout.
pub(crate) fn resolve_client_runtime_paths(
    client_path: Option<PathBuf>,
    explicit_data_dir: Option<PathBuf>,
) -> Result<ClientRuntimePaths> {
    let Some(client_path) = client_path else {
        return Ok(ClientRuntimePaths {
            catalog_path: None,
            client_data_dir: explicit_data_dir,
        });
    };

    let (catalog_path, inferred_data_dir) = infer_client_layout(&client_path);
    require_catalog_file(&client_path, &catalog_path)?;
    let inferred_data_dir = if explicit_data_dir.is_some() {
        None
    } else {
        inferred_data_dir
            .map(existing_data_directory)
            .transpose()?
            .flatten()
    };

    Ok(ClientRuntimePaths {
        catalog_path: Some(catalog_path),
        client_data_dir: explicit_data_dir.or(inferred_data_dir),
    })
}

fn require_catalog_file(client_path: &Path, catalog_path: &Path) -> Result<()> {
    match fs::metadata(catalog_path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(anyhow!(
            "client path {} resolves to {}, which is not a regular KartCatalog.xml file",
            client_path.display(),
            catalog_path.display()
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Err(anyhow!(
            "client path {} resolves to {}, but the C# server has not exported that file yet. \
             Run the C# server's kart-data XML extraction once in the stock client directory, \
             then select the client directory or its Profile directory.",
            client_path.display(),
            catalog_path.display()
        )),
        Err(error) => Err(error)
            .with_context(|| format!("failed to inspect catalog XML {}", catalog_path.display())),
    }
}

fn existing_data_directory(path: PathBuf) -> Result<Option<PathBuf>> {
    match fs::metadata(&path) {
        Ok(metadata) if metadata.is_dir() => Ok(Some(path)),
        Ok(_) => Err(anyhow!(
            "inferred client Data path {} is not a directory",
            path.display()
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error)
            .with_context(|| format!("failed to inspect client Data directory {}", path.display())),
    }
}

fn infer_client_layout(input: &Path) -> (PathBuf, Option<PathBuf>) {
    if has_extension(input, "xml") {
        let data_dir = input
            .parent()
            .filter(|parent| file_name_is(parent, "Profile"))
            .and_then(Path::parent)
            .map(|root| root.join("Data"));
        return (input.to_owned(), data_dir);
    }

    if file_name_is(input, "Profile") {
        let data_dir = input.parent().map(|root| root.join("Data"));
        return (input.join("KartCatalog.xml"), data_dir);
    }

    (
        input.join("Profile").join("KartCatalog.xml"),
        Some(input.join("Data")),
    )
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
    fn client_root_resolves_the_csharp_profile_catalog_and_data_directory() {
        let root = tempdir().unwrap();
        let profile = root.path().join("Profile");
        let catalog = profile.join("KartCatalog.xml");
        let data = root.path().join("Data");
        fs::create_dir(&profile).unwrap();
        fs::write(&catalog, "<KartCatalog />").unwrap();
        fs::create_dir(&data).unwrap();

        let paths = resolve_client_runtime_paths(Some(root.path().to_owned()), None).unwrap();

        assert_eq!(paths.catalog_path.as_deref(), Some(catalog.as_path()));
        assert_eq!(paths.client_data_dir.as_deref(), Some(data.as_path()));
    }

    #[test]
    fn profile_directory_resolves_the_catalog_and_sibling_data_directory() {
        let root = tempdir().unwrap();
        let profile = root.path().join("Profile");
        let catalog = profile.join("KartCatalog.xml");
        fs::create_dir(&profile).unwrap();
        fs::write(&catalog, "<KartCatalog />").unwrap();

        let paths = resolve_client_runtime_paths(Some(profile), None).unwrap();

        assert_eq!(paths.catalog_path.as_deref(), Some(catalog.as_path()));
        assert_eq!(paths.client_data_dir, None);
    }

    #[test]
    fn explicit_data_override_does_not_inspect_a_bad_inferred_sibling() {
        let root = tempdir().unwrap();
        let profile = root.path().join("Profile");
        let catalog = profile.join("KartCatalog.xml");
        let invalid_sibling_data = root.path().join("Data");
        let override_data = root.path().join("alternate-data");
        fs::create_dir(&profile).unwrap();
        fs::write(&catalog, "<KartCatalog />").unwrap();
        fs::write(&invalid_sibling_data, "not a directory").unwrap();
        fs::create_dir(&override_data).unwrap();

        let paths =
            resolve_client_runtime_paths(Some(root.path().to_owned()), Some(override_data.clone()))
                .unwrap();

        assert_eq!(paths.catalog_path.as_deref(), Some(catalog.as_path()));
        assert_eq!(paths.client_data_dir, Some(override_data));
    }

    #[test]
    fn missing_csharp_export_reports_the_expected_catalog_file_without_os_error_text() {
        let root = tempdir().unwrap();

        let error = resolve_client_runtime_paths(Some(root.path().to_owned()), None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("Profile"));
        assert!(error.contains("KartCatalog.xml"));
        assert!(error.contains("kart-data XML extraction"));
    }
}
