//! Durable exact-path and inode holds for user-restored names.
//! A hold is committed before the corresponding pending operation is cleared.
use crate::journal::{atomic_json, sync_directory};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};
const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_HOLDS: usize = 10_000;
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    entries: BTreeMap<String, [u64; 2]>,
}

pub(crate) struct RestoreHolds {
    path: Option<PathBuf>,
    state: State,
}
impl RestoreHolds {
    pub fn load(pending: Option<&Path>) -> Result<Self> {
        let path = pending.map(|path| path.with_file_name("restore-holds.json"));
        let mut state = State {
            version: 1,
            entries: BTreeMap::new(),
        };
        if let Some(path) = &path {
            match File::open(path) {
                Ok(file) => {
                    let mut bytes = Vec::new();
                    file.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes)?;
                    ensure!(
                        bytes.len() <= MAX_BYTES,
                        "restore holds exceed the read limit"
                    );
                    state = serde_json::from_slice(&bytes)?;
                    ensure!(
                        state.version == 1 && state.entries.len() <= MAX_HOLDS,
                        "unsupported restore hold state"
                    );
                    ensure!(
                        state
                            .entries
                            .keys()
                            .all(|path| Path::new(path).is_absolute() && !path.contains('\0')),
                        "invalid restore hold path"
                    );
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(Self { path, state })
    }
    fn save(&self) -> Result<()> {
        if let Some(path) = &self.path {
            ensure!(
                serde_json::to_vec(&self.state)?.len() < MAX_BYTES,
                "restore holds exceed the write limit"
            );
            atomic_json(path, &self.state)?;
            sync_directory(path.parent().unwrap_or(Path::new(".")))?;
        }
        Ok(())
    }
    pub fn insert(&mut self, path: &str, identity: [u64; 2]) -> Result<()> {
        ensure!(self.path.is_some(), "restore hold persistence is required");
        ensure!(
            self.state.entries.contains_key(path) || self.state.entries.len() < MAX_HOLDS,
            "restore hold capacity reached; pending recovery preserved"
        );
        self.state.entries.insert(path.into(), identity);
        self.save()
    }
    pub fn suppresses(&mut self, path: &str, identity: [u64; 2]) -> Result<bool> {
        let before = self.state.entries.len();
        // Other hardlinks can share this identity. Only a replacement observed
        // at the held path invalidates that path's manual naming decision.
        self.state
            .entries
            .retain(|held, id| held != path || *id == identity);
        if self.state.entries.len() != before {
            self.save()?;
        }
        Ok(self.state.entries.get(path) == Some(&identity))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn observing_a_hardlink_keeps_the_original_name_hold() -> Result<()> {
        let base = tempfile::tempdir()?;
        let original = base.path().join("restored");
        let alternate = base.path().join("alternate");
        std::fs::write(&original, b"fixture")?;
        std::fs::hard_link(&original, &alternate)?;
        let metadata = original.metadata()?;
        let identity = [metadata.dev(), metadata.ino()];
        let pending = base.path().join("pending.json");
        let mut holds = RestoreHolds::load(Some(&pending))?;
        holds.insert(original.to_str().unwrap(), identity)?;
        assert!(!holds.suppresses(alternate.to_str().unwrap(), identity)?);
        assert!(holds.suppresses(original.to_str().unwrap(), identity)?);
        assert!(
            RestoreHolds::load(Some(&pending))?.suppresses(original.to_str().unwrap(), identity)?
        );
        assert!(!holds.suppresses(original.to_str().unwrap(), [identity[0], identity[1] + 1])?);
        Ok(())
    }
}
