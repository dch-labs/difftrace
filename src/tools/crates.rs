//! Workspace crate mapping: changed paths to the crates that own them.

use std::path::Path;

use crate::contracts::Region;
use crate::error::DtError;

#[derive(Debug, Clone, serde::Deserialize)]
struct Manifest {
    package: Option<Package>,
    workspace: Option<Workspace>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct Package {
    name: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct Workspace {
    members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemberEntry {
    dir: String,
    crate_name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceMap {
    root: Option<String>,
    members: Vec<MemberEntry>,
}

impl WorkspaceMap {
    #[must_use]
    pub fn new(root: Option<String>, members: Vec<(String, String)>) -> Self {
        let mut entries: Vec<MemberEntry> = members
            .into_iter()
            .map(|(dir, crate_name)| MemberEntry { dir, crate_name })
            .collect();
        entries.sort_by(|a, b| b.dir.len().cmp(&a.dir.len()).then(a.dir.cmp(&b.dir)));
        Self {
            root,
            members: entries,
        }
    }

    pub fn load(root_dir: &Path) -> Result<Self, DtError> {
        let manifest_path = root_dir.join("Cargo.toml");
        let manifest = read_manifest(&manifest_path)?;
        let mut members = Vec::new();
        for entry in manifest.workspace.map(|ws| ws.members).unwrap_or_default() {
            for dir in expand_member(root_dir, &entry)? {
                let member = read_manifest(&root_dir.join(&dir).join("Cargo.toml"))?;
                if let Some(package) = member.package {
                    members.push((dir, package.name));
                }
            }
        }
        Ok(Self::new(
            manifest.package.map(|package| package.name),
            members,
        ))
    }

    #[must_use]
    pub fn crates_for_paths(&self, paths: &[String]) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for path in paths {
            let owner = self
                .members
                .iter()
                .find(|member| {
                    path.starts_with(&member.dir)
                        && path.as_bytes().get(member.dir.len()) == Some(&b'/')
                })
                .map(|member| member.crate_name.clone())
                .or_else(|| self.root.clone());
            if let Some(name) = owner
                && !names.contains(&name)
            {
                names.push(name);
            }
        }
        names.sort_unstable();
        names
    }
}

#[must_use]
pub fn affected_crates(changed: &[Region], ws: &WorkspaceMap) -> Vec<String> {
    let paths = changed
        .iter()
        .map(|region| region.file.clone())
        .collect::<Vec<_>>();
    ws.crates_for_paths(&paths)
}

fn read_manifest(path: &Path) -> Result<Manifest, DtError> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| DtError::Config(format!("cannot read {}: {err}", path.display())))?;
    toml::from_str(&text)
        .map_err(|err| DtError::Config(format!("{} is not a manifest: {err}", path.display())))
}

fn expand_member(root_dir: &Path, entry: &str) -> Result<Vec<String>, DtError> {
    if !entry.contains('*') {
        return Ok(vec![entry.to_owned()]);
    }
    let unsupported = || {
        DtError::Config(format!(
            "unsupported workspace member {entry:?}; only a trailing * is supported"
        ))
    };
    let Some(prefix) = entry.strip_suffix('*') else {
        return Err(unsupported());
    };
    if prefix.contains('*') {
        return Err(unsupported());
    }
    let scan = root_dir.join(prefix);
    let mut dirs = Vec::new();
    let entries = std::fs::read_dir(&scan)
        .map_err(|err| DtError::Config(format!("cannot list {}: {err}", scan.display())))?;
    for candidate in entries {
        let Ok(candidate) = candidate else {
            continue;
        };
        let path = candidate.path();
        if path.join("Cargo.toml").is_file()
            && let Some(name) = path.file_name().and_then(|name| name.to_str())
        {
            dirs.push(format!("{prefix}{name}"));
        }
    }
    dirs.sort_unstable();
    Ok(dirs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::RegionKind;

    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("difftrace-ws-{}-{name}", std::process::id()))
    }

    #[test]
    fn changed_paths_map_to_their_workspace_crates() -> Result<(), Box<dyn std::error::Error>> {
        let root = scratch("map");
        let _unused = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("crates/one/src"))?;
        std::fs::create_dir_all(root.join("crates/two"))?;
        std::fs::create_dir_all(root.join("crates/not-a-crate"))?;
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n\n[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )?;
        std::fs::write(
            root.join("crates/one/Cargo.toml"),
            "[package]\nname = \"one\"\nversion = \"0.1.0\"\n",
        )?;
        std::fs::write(
            root.join("crates/two/Cargo.toml"),
            "[package]\nname = \"renamed-two\"\nversion = \"0.1.0\"\n",
        )?;
        let map = WorkspaceMap::load(&root)?;
        let _unused = std::fs::remove_dir_all(&root);
        let paths = [
            "crates/one/src/lib.rs",
            "crates/two/x.rs",
            "src/main.rs",
            "docs/guide.md",
            "crates/not-a-crate/README",
        ]
        .iter()
        .map(|path| (*path).to_owned())
        .collect::<Vec<_>>();
        assert_eq!(
            map.crates_for_paths(&paths),
            vec!["app", "one", "renamed-two"],
            "member dirs win by prefix; glob dirs without a manifest and unknown paths fall back to the root package, deduped and sorted"
        );
        let region = Region {
            file: "crates/one/src/lib.rs".to_owned(),
            start: 1,
            end: 4,
            kind: RegionKind::Added,
        };
        assert_eq!(
            affected_crates(std::slice::from_ref(&region), &map),
            vec!["one"]
        );
        Ok(())
    }

    #[test]
    fn a_non_trailing_workspace_glob_rejects() -> Result<(), Box<dyn std::error::Error>> {
        let root = scratch("glob");
        let _unused = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root)?;
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*/src\"]\n",
        )?;
        let outcome = WorkspaceMap::load(&root);
        let _unused = std::fs::remove_dir_all(&root);
        assert!(
            outcome.is_err_and(|err| err.to_string().contains("trailing *")),
            "a glob shape the expander cannot read rejects instead of mapping nothing"
        );
        Ok(())
    }

    #[test]
    fn a_virtual_workspace_maps_unknown_paths_to_nothing() {
        let map = WorkspaceMap::new(None, vec![("crates/one".to_owned(), "one".to_owned())]);
        let paths = vec!["docs/guide.md".to_owned()];
        assert!(map.crates_for_paths(&paths).is_empty());
        let rust_paths = vec!["crates/one/src/lib.rs".to_owned()];
        assert_eq!(map.crates_for_paths(&rust_paths), vec!["one"]);
    }
}
