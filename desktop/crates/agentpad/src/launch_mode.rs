use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Mode {
    Normal,
    Admin,
}

impl Mode {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "normal" => Some(Self::Normal),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }

    pub(crate) fn argument(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Admin => "admin",
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Preference {
    schema: u8,
    channel: String,
    mode: Mode,
}

pub(crate) fn decode(bytes: Option<&[u8]>, channel: &str) -> Mode {
    bytes
        .filter(|bytes| bytes.len() <= 256)
        .and_then(|bytes| serde_json::from_slice::<Preference>(bytes).ok())
        .filter(|value| value.schema == 1 && value.channel == channel)
        .map_or(Mode::Normal, |value| value.mode)
}

pub(crate) fn save_at(path: &Path, mode: Mode, channel: &str) -> std::io::Result<()> {
    use std::io::Write;
    let bytes = serde_json::to_vec(&Preference {
        schema: 1,
        channel: channel.into(),
        mode,
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        #[cfg(windows)]
        unsafe {
            use windows::{
                core::HSTRING,
                Win32::Storage::FileSystem::{
                    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
                },
            };
            MoveFileExW(
                &HSTRING::from(temporary.as_os_str()),
                &HSTRING::from(path.as_os_str()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
            .map_err(std::io::Error::other)?;
        }
        #[cfg(not(windows))]
        std::fs::rename(&temporary, path)?;
        if std::fs::read(path)? != bytes {
            return Err(std::io::Error::other("launch preference did not persist"));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_or_other_channel_preferences_default_to_normal() {
        assert_eq!(decode(None, "stable"), Mode::Normal);
        for bytes in [
            b"invalid".as_slice(),
            br#"{"schema":2,"channel":"stable","mode":"admin"}"#,
            br#"{"schema":1,"channel":"beta","mode":"admin"}"#,
            br#"{"schema":1,"channel":"stable","mode":"admin --anything"}"#,
            br#"{"schema":1,"channel":"stable","mode":"admin","path":"other.exe"}"#,
        ] {
            assert_eq!(decode(Some(bytes), "stable"), Mode::Normal);
        }
        assert_eq!(Mode::parse("admin --anything"), None);
        assert_eq!(Mode::parse(Mode::Admin.argument()), Some(Mode::Admin));
        assert_eq!(Mode::parse(Mode::Normal.argument()), Some(Mode::Normal));
    }

    #[test]
    fn remembers_both_modes_and_failed_replace_preserves_previous_choice() {
        let dir = std::env::temp_dir().join(format!("agentpad-mode-{}", uuid::Uuid::new_v4()));
        let path = dir.join("launch-mode.json");
        for mode in [Mode::Admin, Mode::Normal] {
            save_at(&path, mode, "stable").unwrap();
            assert_eq!(decode(Some(&std::fs::read(&path).unwrap()), "stable"), mode);
            assert_eq!(
                decode(Some(&std::fs::read(&path).unwrap()), "beta"),
                Mode::Normal
            );
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let locked = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(1)
                .open(&path)
                .unwrap();
            assert!(save_at(&path, Mode::Admin, "stable").is_err());
            assert_eq!(
                decode(Some(&std::fs::read(&path).unwrap()), "stable"),
                Mode::Normal
            );
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
            drop(locked);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
