use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

#[cfg(test)]
thread_local! { pub(crate) static FORBID_FILESYSTEM: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

pub const TMP_SUFFIX: &str = ".__jaso_nfc_tmp__";
pub fn nfc(value: &str) -> String {
    value.nfc().collect()
}
pub fn within(path: &str, root: &str) -> bool {
    let (p, r) = (nfc(path), nfc(root));
    p == r || p.starts_with(&(r.trim_end_matches('/').to_owned() + "/"))
}
pub fn absolute(path: &str) -> String {
    let full = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()
            .expect("working directory")
            .join(path)
    };
    let mut result = PathBuf::new();
    for part in full.components() {
        match part {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            _ => result.push(part.as_os_str()),
        }
    }
    result
        .to_str()
        .expect("input and current directory must be Unicode")
        .to_owned()
}

// Google Drive's provider-root staging store is infrastructure, not the user's
// document tree. Do not generalize this to hidden folders inside My Drive or to
// similarly named directories elsewhere on disk.
pub fn is_managed_cloud_path(path: &str) -> bool {
    Path::new(path).ancestors().any(|part| {
        if part.file_name().is_none_or(|name| name != ".tmp") {
            return false;
        }
        let Some(provider) = part.parent() else {
            return false;
        };
        if !provider
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.starts_with("GoogleDrive-") && name.len() > "GoogleDrive-".len()
            })
        {
            return false;
        }
        provider.parent().is_some_and(|cloud| {
            let components: Vec<_> = cloud.components().collect();
            let home_layout = |parts: &[Component<'_>]| {
                parts.len() == 4
                    && parts[0].as_os_str() == "Users"
                    && parts[2].as_os_str() == "Library"
                    && parts[3].as_os_str() == "CloudStorage"
            };
            // Anchor at an account home, never a copied hierarchy inside a
            // document folder. Mounted macOS home trees use /Volumes/X/Users.
            components.first() == Some(&Component::RootDir)
                && (home_layout(&components[1..])
                    || (components.len() == 7
                        && components[1].as_os_str() == "Volumes"
                        && home_layout(&components[3..])))
        })
    })
}

#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub roots: Vec<String>,
    pub excludes: Vec<String>,
    pub exclude_names: Vec<String>,
    pub skip_hidden_tops: Vec<String>,
    pub root_excludes: HashMap<String, Vec<String>>,
}
impl Policy {
    pub fn new(
        roots: Vec<String>,
        excludes: Vec<String>,
        exclude_names: Vec<String>,
        skip_hidden_tops: Vec<String>,
        root_excludes: HashMap<String, Vec<String>>,
    ) -> Self {
        let mut unique = Vec::new();
        for root in roots {
            let root = absolute(&root);
            if !unique.contains(&root) {
                unique.push(root);
            }
        }
        Self {
            roots: unique,
            excludes: excludes.iter().map(|p| absolute(p)).collect(),
            exclude_names: exclude_names.iter().map(|p| nfc(p)).collect(),
            skip_hidden_tops: skip_hidden_tops.iter().map(|p| absolute(p)).collect(),
            root_excludes: root_excludes
                .into_iter()
                .map(|(r, p)| (nfc(&absolute(&r)), p.iter().map(|p| absolute(p)).collect()))
                .collect(),
        }
    }
    pub fn root_for(&self, path: &str) -> Option<String> {
        self.roots
            .iter()
            .filter(|r| within(path, r))
            .max_by_key(|r| nfc(r).len())
            .cloned()
    }
    pub fn contains(&self, path: &str) -> bool {
        self.root_for(&absolute(path)).is_some()
    }
    pub fn accepts(&self, path: &str) -> bool {
        self.accepts_impl(path, true)
    }
    /// Scheduling filter without filesystem access; reconcile still validates
    /// symlink, mount, and package boundaries before processing the path.
    pub fn accepts_lexically(&self, path: &str) -> bool {
        self.accepts_impl(path, false)
    }
    fn accepts_impl(&self, path: &str, check_filesystem: bool) -> bool {
        let path = absolute(path);
        if is_managed_cloud_path(&path) {
            return false;
        }
        let Some(root) = self.root_for(&path) else {
            return false;
        };
        if self.excludes.iter().any(|r| within(&path, r)) {
            return false;
        }
        if self
            .root_excludes
            .get(&nfc(&root))
            .is_some_and(|v| v.iter().any(|r| within(&path, r)))
        {
            return false;
        }
        for top in &self.skip_hidden_tops {
            if within(&path, top)
                && nfc(&path) != nfc(top)
                && Path::new(&path)
                    .components()
                    .nth(Path::new(top).components().count())
                    .is_some_and(|p| p.as_os_str().as_encoded_bytes().starts_with(b"."))
            {
                return false;
            }
        }
        let boundary = self
            .roots
            .iter()
            .filter(|r| within(&path, r))
            .min_by_key(|r| nfc(r).len())
            .unwrap();
        let mut ancestor = PathBuf::from(boundary);
        for part in Path::new(&path)
            .components()
            .skip(Path::new(boundary).components().count())
        {
            let name = part.as_os_str().to_str().expect("Unicode input");
            ancestor.push(name);
            if self.exclude_names.contains(&nfc(name))
                || name.ends_with(TMP_SUFFIX)
                || name.starts_with("._")
            {
                return false;
            }
            let ancestor = ancestor.to_str().unwrap();
            if check_filesystem
                && nfc(ancestor) != nfc(&path)
                && Self::blocked_directory(ancestor, self.is_root(ancestor))
            {
                return false;
            }
        }
        !check_filesystem || nfc(&path) == nfc(boundary) || !Self::blocked_directory(boundary, true)
    }
    fn is_root(&self, path: &str) -> bool {
        self.roots.iter().any(|r| nfc(r) == nfc(path))
    }
    pub fn blocked_directory(path: &str, configured_root: bool) -> bool {
        #[cfg(test)]
        assert!(
            !FORBID_FILESYSTEM.get(),
            "filesystem policy called inside index operation: {path}"
        );
        let Ok(info) = crate::directory_io::metadata(Path::new(path), false) else {
            return false;
        };
        if info.st_mode as u32 & libc::S_IFMT as u32 != libc::S_IFDIR as u32 {
            return true;
        }
        let extension = Path::new(path)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        if matches!(
            extension.as_str(),
            "app" | "photoslibrary" | "musiclibrary" | "tvlibrary" | "aplibrary" | "framework"
        ) {
            return true;
        }
        if configured_root {
            return false;
        }
        let parent = Path::new(path).parent().unwrap_or(Path::new("/"));
        crate::directory_io::metadata(parent, true).is_ok_and(|p| {
            p.st_dev != info.st_dev || (p.st_dev == info.st_dev && p.st_ino == info.st_ino)
        })
    }
    pub fn descend(&self, path: &str) -> bool {
        let path = absolute(path);
        self.accepts(&path) && !Self::blocked_directory(&path, self.is_root(&path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_cloud_overrides_only_scoped_exclusion() {
        let p = Policy::new(
            vec!["/tmp/user".into(), "/tmp/user/Library/CloudStorage".into()],
            vec!["/tmp/user/Library/CloudStorage/private".into()],
            vec![".git".into()],
            vec![],
            HashMap::from([("/tmp/user".into(), vec!["/tmp/user/Library".into()])]),
        );
        assert!(p.accepts("/tmp/user/Library/CloudStorage/file"));
        assert!(!p.accepts("/tmp/user/Library/other"));
        assert!(!p.accepts("/tmp/user/Library/CloudStorage/private/file"));
        assert!(!p.accepts("/tmp/user/Library/CloudStorage/.git/file"));
    }
    #[test]
    fn nested_root_cannot_override_symlink_or_package_boundaries() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().to_str().unwrap();
        let package = d.path().join("Editor.app");
        std::fs::create_dir(&package).unwrap();
        let child = package.join("data");
        std::fs::create_dir(&child).unwrap();
        let p = Policy::new(
            vec![root.into(), child.to_str().unwrap().into()],
            vec![],
            vec![],
            vec![],
            HashMap::new(),
        );
        assert!(!p.accepts(child.join("file").to_str().unwrap()));
    }

    #[test]
    fn google_drive_staging_is_not_user_content_even_with_explicit_nested_root() {
        let cloud = "/Users/example/Library/CloudStorage";
        let staging = format!("{cloud}/GoogleDrive-account/.tmp");
        let p = Policy::new(
            vec![cloud.into(), format!("{staging}/205")],
            vec![],
            vec![],
            vec![],
            HashMap::new(),
        );
        assert!(!p.accepts(&staging), "provider staging must not be renamed");
        assert!(!p.accepts(&format!("{staging}/205/document")));
        assert!(!p.descend(&format!("{staging}/205")));
        for suffix in [
            "My Drive/document",
            "My Drive/.tmp/document",
            "Shared drives/.tmp/document",
            ".shortcut-targets-by-id/123/document",
            ".tmp-not-staging/document",
        ] {
            assert!(
                p.accepts(&format!("{cloud}/GoogleDrive-account/{suffix}")),
                "user content remains covered: {suffix}"
            );
        }
        assert!(p.accepts(&format!("{cloud}/OtherProvider/.tmp/document")));
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_str().unwrap();
        let p = Policy::new(vec![root.into()], vec![], vec![], vec![], HashMap::new());
        assert!(p.accepts(&format!("{root}/GoogleDrive-account/.tmp/document")));
        assert!(!is_managed_cloud_path(
            "/Users/example/Documents/archive/Library/CloudStorage/GoogleDrive-copy/.tmp/user-file"
        ));
        assert!(!is_managed_cloud_path(
            "/Volumes/Data/archive/Users/example/Library/CloudStorage/GoogleDrive-copy/.tmp/user-file"
        ));
        assert!(is_managed_cloud_path(
            "/Volumes/OtherMac/Users/example/Library/CloudStorage/GoogleDrive-account/.tmp/205/item"
        ));
    }
}
