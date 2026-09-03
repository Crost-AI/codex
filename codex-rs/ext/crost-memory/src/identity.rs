//! Project identity resolved from committed repository content.
//!
//! Identity NEVER depends on the checkout path, the worktree name, `$USER`, or
//! the current branch. `.crost/project.yaml` is committed content, so every
//! clone and every worktree of the same repository resolves to the same
//! identity.

use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

/// Default location of the committed project descriptor.
pub const DEFAULT_PROJECT_FILE: &str = ".crost/project.yaml";

/// Only accepted `apiVersion` value.
pub const SUPPORTED_API_VERSION: &str = "memory.crost/v1";

/// Immutable identity of one Crost project.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProjectIdentity {
    pub project_id: String,
    pub slug: String,
    pub bank_prefix: Option<String>,
}

impl ProjectIdentity {
    /// Bank-name prefix derived from the descriptor.
    pub fn bank_prefix(&self) -> String {
        match self.bank_prefix.as_deref() {
            Some(prefix) if !prefix.trim().is_empty() => prefix.trim().to_string(),
            _ => {
                let slug = &self.slug;
                format!("crost--{slug}")
            }
        }
    }
}

/// Precise reason no identity could be resolved, for diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdentityError {
    /// No ancestor of the start directory contains the descriptor.
    NotFound {
        searched_from: PathBuf,
        file: String,
    },
    /// The descriptor exists but could not be read.
    Unreadable { path: PathBuf, detail: String },
    /// The descriptor exists but is not usable.
    Invalid { path: PathBuf, detail: String },
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound {
                searched_from,
                file,
            } => {
                let searched_from = searched_from.display();
                write!(
                    f,
                    "no `{file}` found in `{searched_from}` or any ancestor directory"
                )
            }
            Self::Unreadable { path, detail } => {
                let path = path.display();
                write!(f, "could not read `{path}`: {detail}")
            }
            Self::Invalid { path, detail } => {
                let path = path.display();
                write!(f, "`{path}` is not a valid project descriptor: {detail}")
            }
        }
    }
}

/// Resolves project identity using the default descriptor path.
///
/// Returns `None` when memory must stay disabled for the session.
pub fn resolve_project_identity(start_dir: &Path) -> Option<ProjectIdentity> {
    resolve_project_identity_at(start_dir, DEFAULT_PROJECT_FILE).ok()
}

/// Resolves project identity, reporting the precise failure reason.
pub fn resolve_project_identity_at(
    start_dir: &Path,
    project_file: &str,
) -> Result<ProjectIdentity, IdentityError> {
    let relative = project_file.trim();
    let relative = if relative.is_empty() {
        DEFAULT_PROJECT_FILE
    } else {
        relative
    };

    let mut current = Some(start_dir);
    while let Some(dir) = current {
        let candidate = dir.join(relative);
        if candidate.is_file() {
            let contents =
                std::fs::read_to_string(&candidate).map_err(|err| IdentityError::Unreadable {
                    path: candidate.clone(),
                    detail: err.to_string(),
                })?;
            return parse_project_identity(&contents).map_err(|detail| IdentityError::Invalid {
                path: candidate,
                detail,
            });
        }
        current = dir.parent();
    }

    Err(IdentityError::NotFound {
        searched_from: start_dir.to_path_buf(),
        file: relative.to_string(),
    })
}

/// Parses the minimal YAML subset used by `.crost/project.yaml`.
///
/// Supported syntax is intentionally tiny: `key: value` lines, `#` comments,
/// optional single or double quotes, and a leading `---` document marker.
pub fn parse_project_identity(contents: &str) -> Result<ProjectIdentity, String> {
    let mut api_version: Option<String> = None;
    let mut project_id: Option<String> = None;
    let mut slug: Option<String> = None;
    let mut bank_prefix: Option<String> = None;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line == "---" || line == "..." {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let Some(value) = parse_scalar(value) else {
            continue;
        };
        match key {
            "apiVersion" => api_version = Some(value),
            "projectId" => project_id = Some(value),
            "slug" => slug = Some(value),
            "bankPrefix" => bank_prefix = Some(value),
            _ => {}
        }
    }

    if let Some(api_version) = api_version.as_deref()
        && api_version != SUPPORTED_API_VERSION
    {
        return Err(format!(
            "unsupported apiVersion `{api_version}` (expected `{SUPPORTED_API_VERSION}`)"
        ));
    }

    let project_id = project_id.filter(|value| !value.is_empty());
    let Some(project_id) = project_id else {
        return Err("missing or empty `projectId`".to_string());
    };
    if project_id == "."
        || project_id == ".."
        || project_id.contains('/')
        || project_id.contains('\\')
    {
        return Err(format!(
            "`projectId` value `{project_id}` must not be a path component"
        ));
    }
    let Some(slug) = slug else {
        return Err("missing `slug`".to_string());
    };
    if !is_valid_slug(&slug) {
        return Err(format!(
            "`slug` value `{slug}` does not match [a-z0-9][a-z0-9._-]*"
        ));
    }
    let bank_prefix = bank_prefix.filter(|value| !value.is_empty());

    Ok(ProjectIdentity {
        project_id,
        slug,
        bank_prefix,
    })
}

fn parse_scalar(raw: &str) -> Option<String> {
    let value = raw.trim();
    for quote in ['"', '\''] {
        if let Some(rest) = value.strip_prefix(quote)
            && let Some(inner) = rest.split(quote).next()
        {
            return Some(inner.to_string());
        }
    }
    let value = match value.split_once('#') {
        Some((before, _)) => before.trim(),
        None => value,
    };
    if value.is_empty() {
        return None;
    }
    Some(value.to_string())
}

fn is_valid_slug(slug: &str) -> bool {
    let mut chars = slug.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const DESCRIPTOR: &str = "# committed project descriptor\n\
         apiVersion: memory.crost/v1\n\
         projectId: 01J8ZQ0000000000000000000\n\
         slug: ohm-storefront\n";

    fn write_descriptor(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|err| panic!("create dirs: {err}"));
        }
        std::fs::write(&path, contents).unwrap_or_else(|err| panic!("write descriptor: {err}"));
    }

    #[test]
    fn parses_minimal_descriptor() {
        let identity = parse_project_identity(DESCRIPTOR).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(
            identity,
            ProjectIdentity {
                project_id: "01J8ZQ0000000000000000000".to_string(),
                slug: "ohm-storefront".to_string(),
                bank_prefix: None,
            }
        );
        assert_eq!(identity.bank_prefix(), "crost--ohm-storefront");
    }

    #[test]
    fn honours_explicit_bank_prefix_and_quotes() {
        let identity = parse_project_identity(
            "projectId: \"abc\"  # inline comment\nslug: 'ohm'\nbankPrefix: crost--custom\n",
        )
        .unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(identity.project_id, "abc");
        assert_eq!(identity.slug, "ohm");
        assert_eq!(identity.bank_prefix(), "crost--custom");
    }

    #[test]
    fn rejects_unsupported_api_version() {
        let err = parse_project_identity("apiVersion: memory.crost/v2\nprojectId: a\nslug: b\n")
            .expect_err("v2 must be rejected");

        assert!(err.contains("unsupported apiVersion"));
    }

    #[test]
    fn rejects_missing_and_malformed_fields() {
        assert!(parse_project_identity("slug: ohm\n").is_err());
        assert!(parse_project_identity("projectId: a\n").is_err());
        assert!(parse_project_identity("projectId: a\nslug: Ohm\n").is_err());
        assert!(parse_project_identity("projectId: a\nslug: -ohm\n").is_err());
        assert!(parse_project_identity("projectId: \nslug: ohm\n").is_err());
    }

    #[test]
    fn walks_ancestors_from_the_start_directory() {
        let tmp = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        write_descriptor(tmp.path(), DEFAULT_PROJECT_FILE, DESCRIPTOR);
        let nested = tmp.path().join("crates/deep/nested");
        std::fs::create_dir_all(&nested).unwrap_or_else(|err| panic!("create nested: {err}"));

        let identity = resolve_project_identity(&nested).unwrap_or_else(|| panic!("identity"));

        assert_eq!(identity.slug, "ohm-storefront");
    }

    #[test]
    fn missing_descriptor_reports_not_found() {
        let tmp = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));

        let err = resolve_project_identity_at(tmp.path(), DEFAULT_PROJECT_FILE)
            .expect_err("no descriptor exists");

        assert!(matches!(err, IdentityError::NotFound { .. }));
        assert!(err.to_string().contains(".crost/project.yaml"));
        assert!(resolve_project_identity(tmp.path()).is_none());
    }

    #[test]
    fn invalid_descriptor_reports_precise_reason() {
        let tmp = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        write_descriptor(tmp.path(), DEFAULT_PROJECT_FILE, "slug: ohm\n");

        let err = resolve_project_identity_at(tmp.path(), DEFAULT_PROJECT_FILE)
            .expect_err("descriptor is invalid");

        assert!(matches!(err, IdentityError::Invalid { .. }));
        assert!(err.to_string().contains("projectId"));
    }

    #[test]
    fn worktrees_with_the_same_committed_file_resolve_identically() {
        // Two independent directory trees stand in for two worktrees/clones of
        // the same repository. The descriptor is committed content, so both
        // resolve to the same identity even though the paths differ.
        let first = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        let second = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        write_descriptor(first.path(), DEFAULT_PROJECT_FILE, DESCRIPTOR);
        write_descriptor(second.path(), DEFAULT_PROJECT_FILE, DESCRIPTOR);
        let first_nested = first.path().join("a/b");
        let second_nested = second.path().join("totally/different/depth");
        std::fs::create_dir_all(&first_nested).unwrap_or_else(|err| panic!("mkdir: {err}"));
        std::fs::create_dir_all(&second_nested).unwrap_or_else(|err| panic!("mkdir: {err}"));

        assert_ne!(first.path(), second.path());
        assert_eq!(
            resolve_project_identity(&first_nested),
            resolve_project_identity(&second_nested)
        );
    }

    #[test]
    fn honours_a_custom_descriptor_path() {
        let tmp = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        write_descriptor(tmp.path(), "custom/identity.yaml", DESCRIPTOR);

        let identity = resolve_project_identity_at(tmp.path(), "custom/identity.yaml")
            .unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(identity.slug, "ohm-storefront");
        assert!(resolve_project_identity_at(tmp.path(), DEFAULT_PROJECT_FILE).is_err());
    }
}
