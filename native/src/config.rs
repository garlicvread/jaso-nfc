use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::unix::{fs::OpenOptionsExt, io::AsRawFd};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DriveReference {
    pub uuid: String,
    /// Last observed mount spelling, retained for disconnected-drive display.
    pub mount: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DriveReconnectPreference {
    pub uuid: String,
    pub mount: String,
    pub mode: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct DrivePreferences {
    pub mode: String,
    pub included: Vec<DriveReference>,
    pub excluded: Vec<DriveReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reconnect: Vec<DriveReconnectPreference>,
}
impl Default for DrivePreferences {
    fn default() -> Self {
        Self {
            mode: "automatic".into(),
            included: vec![],
            excluded: vec![],
            reconnect: vec![],
        }
    }
}
impl DrivePreferences {
    fn validate(&mut self) -> Result<()> {
        ensure!(
            matches!(self.mode.as_str(), "automatic" | "selected"),
            "drive mode must be automatic or selected"
        );
        for references in [&mut self.included, &mut self.excluded] {
            ensure!(references.len() <= 256, "choose no more than 256 drives");
            for reference in references.iter() {
                ensure!(
                    !reference.uuid.trim().is_empty()
                        && !reference.uuid.contains('\0')
                        && reference.uuid.len() <= 256,
                    "drive UUIDs must be nonempty, valid, and no longer than 256 bytes"
                );
                ensure!(
                    !reference.mount.contains('\0')
                        && reference.mount.len() <= 4096
                        && reference
                            .mount
                            .strip_prefix("/Volumes/")
                            .is_some_and(|name| {
                                !name.is_empty()
                                    && !name.contains('/')
                                    && !matches!(name, "." | "..")
                            }),
                    "drive mounts must be direct absolute /Volumes/name paths"
                );
            }
            let mut seen = std::collections::HashSet::new();
            references.retain(|reference| seen.insert(reference.uuid.clone()));
        }
        ensure!(
            !self.included.iter().any(|included| self
                .excluded
                .iter()
                .any(|excluded| excluded.uuid == included.uuid)),
            "a drive cannot be both included and excluded"
        );
        ensure!(
            self.reconnect.len() <= 256,
            "choose no more than 256 reconnect preferences"
        );
        let mut seen = std::collections::HashSet::new();
        for preference in &self.reconnect {
            ensure!(
                matches!(preference.mode.as_str(), "automatic" | "manual"),
                "reconnect mode must be automatic or manual"
            );
            ensure!(
                seen.insert(&preference.uuid),
                "a drive cannot have multiple reconnect preferences"
            );
            let mut reference = Self {
                included: vec![DriveReference {
                    uuid: preference.uuid.clone(),
                    mount: preference.mount.clone(),
                }],
                ..Self::default()
            };
            reference.validate()?;
        }
        Ok(())
    }

    pub fn reconnect_mode(&self, uuid: &str) -> Option<&str> {
        self.reconnect
            .iter()
            .find(|preference| preference.uuid == uuid)
            .map(|preference| preference.mode.as_str())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub scope: String,
    pub roots: Vec<String>,
    pub excludes: Vec<String>,
    pub exclude_names: Vec<String>,
    pub skip_hidden_tops: Vec<String>,
    pub state_dir: String,
    pub log_dir: Option<String>,
    pub apply: bool,
    pub drives: DrivePreferences,
}
impl Default for Config {
    fn default() -> Self {
        let home = std::env::var("HOME").expect("HOME is required");
        Self {
            scope: "configured".into(),
            roots: vec![home.clone(), "/Users/Shared".into()],
            excludes: vec![format!("{home}/Library"), format!("{home}/.Trash")],
            exclude_names: vec![".git".into()],
            skip_hidden_tops: vec![home.clone()],
            state_dir: format!("{home}/Library/Application Support/jaso-nfc"),
            log_dir: None,
            apply: false,
            drives: DrivePreferences::default(),
        }
    }
}
impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let mut config: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&mut self) -> Result<()> {
        self.drives.validate()?;
        ensure!(
            matches!(self.scope.as_str(), "configured" | "all-user-files"),
            "scope must be configured or all-user-files"
        );
        if self.log_dir.is_none() {
            self.log_dir = Some(format!("{}/logs", self.state_dir));
        }
        for paths in [
            &mut self.roots,
            &mut self.excludes,
            &mut self.skip_hidden_tops,
        ] {
            ensure!(
                paths.iter().all(|s| !s.is_empty() && !s.contains('\0')),
                "paths must be nonempty and contain no NUL"
            );
            let mut normalized = Vec::new();
            for path in paths.iter() {
                let value = crate::policy::absolute(&expand_user(path));
                if !normalized.contains(&value) {
                    normalized.push(value);
                }
            }
            *paths = normalized;
        }
        ensure!(
            self.exclude_names
                .iter()
                .all(|s| !s.is_empty() && !s.contains('/') && !s.contains('\0')),
            "exclude_names must contain basenames"
        );
        ensure!(
            !self.state_dir.is_empty() && !self.state_dir.contains('\0'),
            "state_dir must be a nonempty path"
        );
        self.state_dir = crate::policy::absolute(&expand_user(&self.state_dir));
        let log = self.log_dir.as_mut().unwrap();
        ensure!(
            !log.is_empty() && !log.contains('\0'),
            "log_dir must be a nonempty path"
        );
        *log = crate::policy::absolute(&expand_user(log));
        Ok(())
    }
    pub fn signature(&self) -> String {
        let mut value = serde_json::to_value(self).expect("serializable config");
        // Drive coverage changes incrementally; preserve the Python policy
        // identity so selecting a drive never invalidates the complete index.
        value.as_object_mut().unwrap().remove("drives");
        let encoded = python_json(&value);
        format!("{:x}", Sha256::digest(encoded.as_bytes()))
    }
    pub fn runtime_signature(&self) -> String {
        // Worker acknowledgement must include settings excluded from the
        // historical policy signature, including drive selection.
        let encoded = python_json(&serde_json::to_value(self).expect("serializable config"));
        format!("{:x}", Sha256::digest(encoded.as_bytes()))
    }
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        atomic_json(path, self)
    }
    pub fn state_path(&self, name: &str) -> PathBuf {
        Path::new(&self.state_dir).join("state").join(name)
    }
    pub fn policy(&self) -> crate::policy::Policy {
        let mut excludes = self.excludes.clone();
        excludes.push(self.state_dir.clone());
        excludes.push(self.logs());
        crate::policy::Policy::new(
            self.roots.clone(),
            excludes,
            self.exclude_names.clone(),
            self.skip_hidden_tops.clone(),
            Default::default(),
        )
    }
    pub fn logs(&self) -> String {
        self.log_dir
            .clone()
            .unwrap_or_else(|| format!("{}/logs", self.state_dir))
    }
}

fn expand_user(path: &str) -> String {
    if path == "~" || path.starts_with("~/") {
        return std::env::var("HOME")
            .map(|h| h + &path[1..])
            .unwrap_or_else(|_| path.into());
    }
    path.into()
}

// Preserve the Python release's config identity across the native migration.
// Its JSON default uses sorted keys, spaces after separators and ASCII escapes.
fn python_json(value: &serde_json::Value) -> String {
    use serde_json::Value::*;
    match value {
        String(s) => {
            let json = serde_json::to_string(s).unwrap();
            json.chars()
                .map(|c| {
                    if c.is_ascii() {
                        c.to_string()
                    } else {
                        let mut units = [0; 2];
                        c.encode_utf16(&mut units)
                            .iter()
                            .map(|n| format!("\\u{n:04x}"))
                            .collect()
                    }
                })
                .collect()
        }
        Array(a) => format!(
            "[{}]",
            a.iter().map(python_json).collect::<Vec<_>>().join(", ")
        ),
        Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", python_json(&String(k.clone())), python_json(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => value.to_string(),
    }
}

pub fn atomic_json(path: impl AsRef<Path>, value: &impl Serialize) -> Result<()> {
    let path = path.as_ref();
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing state parent"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".state-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.flush()?;
        // POSIX fsync matches the existing durability contract; do not replace
        // with Apple's stronger F_FULLFSYNC implicitly via File::sync_all.
        if unsafe { libc::fsync(file.as_raw_fd()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if temporary.exists() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn config_with_drives(drives: Value) -> Config {
        serde_json::from_value(json!({"drives": drives})).unwrap()
    }

    #[test]
    fn legacy_configuration_defaults_to_automatic_drives() {
        let mut config: Config = serde_json::from_str("{}").unwrap();
        config.validate().unwrap();
        assert_eq!(
            serde_json::to_value(&config).unwrap()["drives"],
            json!({"mode":"automatic", "included":[], "excluded":[]})
        );
    }

    #[test]
    fn per_drive_reconnect_preserves_legacy_policy_and_validates_uuid_choices() {
        let reconnect = json!([{"uuid":"archive", "mount":"/Volumes/Archive", "mode":"manual"}]);
        let mut config = config_with_drives(json!({"reconnect":reconnect}));
        config.validate().unwrap();
        assert_eq!(
            serde_json::to_value(&config).unwrap()["drives"]["reconnect"],
            reconnect
        );
        assert_eq!(config.drives.mode, "automatic");
        for mode in ["", "sometimes"] {
            let mut bad = config_with_drives(
                json!({"reconnect":[{"uuid":"archive", "mount":"/Volumes/Archive", "mode":mode}]}),
            );
            assert!(bad.validate().is_err());
        }
    }

    #[test]
    fn drive_preferences_round_trip_without_changing_policy_identity() {
        let mut legacy: Config = serde_json::from_str("{}").unwrap();
        legacy.validate().unwrap();
        let choices = json!({"mode":"selected", "included":[{"uuid":"volume-a", "mount":"/Volumes/Photos"}], "excluded":[]});
        let mut selected = config_with_drives(choices.clone());
        selected.validate().unwrap();
        assert_eq!(selected.signature(), legacy.signature());
        assert_eq!(serde_json::to_value(&selected).unwrap()["drives"], choices);
        let reloaded: Config =
            serde_json::from_slice(&serde_json::to_vec(&selected).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(reloaded).unwrap()["drives"], choices);
    }

    #[test]
    fn drive_preferences_deduplicate_volume_identity() {
        let mut config = config_with_drives(json!({"mode":"automatic", "included":[
            {"uuid":"volume-a", "mount":"/Volumes/Photos"},
            {"uuid":"volume-a", "mount":"/Volumes/Photos"}
        ], "excluded":[]}));
        config.validate().unwrap();
        assert_eq!(
            serde_json::to_value(&config).unwrap()["drives"]["included"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn drive_preferences_reject_invalid_references_and_conflicting_choices() {
        for reference in [
            json!({"uuid":"", "mount":"/Volumes/Photos"}),
            json!({"uuid":"   ", "mount":"/Volumes/Photos"}),
            json!({"uuid":"volume\0a", "mount":"/Volumes/Photos"}),
            json!({"uuid":"x".repeat(257), "mount":"/Volumes/Photos"}),
            json!({"uuid":"volume-a", "mount":"Volumes/Photos"}),
            json!({"uuid":"volume-a", "mount":"/Volumes"}),
            json!({"uuid":"volume-a", "mount":"/Volumes/Photos/subfolder"}),
            json!({"uuid":"volume-a", "mount":"/Volumes/../Users"}),
            json!({"uuid":"volume-a", "mount":"/Volumes/."}),
            json!({"uuid":"volume-a", "mount":"/Volumes/Photos/"}),
            json!({"uuid":"volume-a", "mount":"/Volumes/Photos\0"}),
            json!({"uuid":"volume-a", "mount":format!("/Volumes/{}", "x".repeat(4096))}),
        ] {
            let mut config = config_with_drives(
                json!({"mode":"selected", "included":[reference.clone()], "excluded":[]}),
            );
            assert!(config.validate().is_err(), "accepted {reference}");
        }
        let reference = json!({"uuid":"volume-a", "mount":"/Volumes/Photos"});
        let mut conflict =
            config_with_drives(json!({"included":[reference.clone()], "excluded":[reference]}));
        assert!(conflict.validate().is_err());
        let mut oversized = config_with_drives(
            json!({"included":(0..257).map(|index| json!({"uuid":format!("volume-{index}"), "mount":"/Volumes/Photos"})).collect::<Vec<_>>()}),
        );
        assert!(oversized.validate().is_err());
        let mut invalid_mode = config_with_drives(json!({"mode":"sometimes"}));
        assert!(invalid_mode.validate().is_err());
        assert!(serde_json::from_value::<Config>(json!({"drives":{"typo":true}})).is_err());
        assert!(
            serde_json::from_value::<Config>(
                json!({"drives":{"included":[{"uuid":"a", "mount":"/Volumes/A", "typo":true}]}})
            )
            .is_err()
        );
    }

    #[test]
    fn configuration_does_not_silently_accept_unknown_fields() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.json");
        std::fs::write(&p, r#"{"apply":true,"scoep":"all-user-files"}"#).unwrap();
        assert!(Config::load(p).is_err());
    }
    #[test]
    fn configured_empty_roots_persist_as_an_idle_configuration() {
        let mut c = Config {
            roots: vec![],
            ..Config::default()
        };
        c.validate().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        c.save(&path).unwrap();
        let reloaded = Config::load(path).unwrap();
        assert_eq!(reloaded.scope, "configured");
        assert!(reloaded.roots.is_empty());
        assert!(reloaded.policy().roots.is_empty());
    }
    #[test]
    fn migration_signature_matches_python() {
        let mut c = Config {
            roots: vec!["/tmp/files".into()],
            excludes: vec![],
            skip_hidden_tops: vec![],
            state_dir: "/tmp/state".into(),
            apply: true,
            ..Config::default()
        };
        c.validate().unwrap();
        assert_eq!(
            c.signature(),
            "62831287c3ab040aa54275bea04a0b86ce9099f4b8c1009201b47f745e809cd4"
        );
        c.roots = vec!["/tmp/한글😀".into()];
        assert_eq!(
            c.signature(),
            "ddfeae1570903b174aa2ca02d346c60c24a8626f1906d2bc37b89a00f56a94ff"
        );
    }
}
