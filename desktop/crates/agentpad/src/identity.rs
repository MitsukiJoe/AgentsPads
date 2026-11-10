use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub device_id: String,
    pub name: String,
    /// 长期配对密钥，随二维码给手机；重置后所有手机需重新配对。
    #[serde(default)]
    pub secret: String,
}

pub fn data_dir() -> PathBuf {
    runtime_dir().unwrap_or_else(fallback_runtime_dir)
}

/// 提权进程的可写运行时目录。拿不到受保护目录时返回 None，调用方不得改走用户目录。
pub(crate) fn runtime_dir() -> Option<PathBuf> {
    let elevated = crate::elevation::is_elevated();
    let protected = elevated_state_dir(elevated);
    select_runtime_dir(
        std::env::var("AGENTPAD_DATA_DIR").ok().as_deref(),
        elevated,
        protected.as_deref(),
        &platform_user_dir(),
    )
}

fn elevated_state_dir(elevated: bool) -> Option<PathBuf> {
    if !elevated {
        return None;
    }
    #[cfg(windows)]
    {
        if !crate::elevation::installed() {
            return None;
        }
        crate::elevation::protected_state_dir()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn fallback_runtime_dir() -> PathBuf {
    #[cfg(windows)]
    {
        unwritable_elevated_dir()
    }
    #[cfg(not(windows))]
    {
        platform_user_dir()
    }
}

#[cfg(windows)]
fn unwritable_elevated_dir() -> PathBuf {
    // Program Files 缺失时的占位。这个路径创建不了，主题、引导和日志会静默跳过。
    PathBuf::from(r"\\.\NUL\AgentsPads\state")
}

/// 普通用户数据目录。提权进程只把它用于过滤后的只读，并且忽略 AGENTPAD_DATA_DIR。
pub(crate) fn fixed_user_data_dir() -> PathBuf {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Shell::{
            FOLDERID_RoamingAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
        };
        unsafe {
            SHGetKnownFolderPath(&FOLDERID_RoamingAppData, KF_FLAG_DEFAULT, None)
                .ok()
                .and_then(|value| {
                    let result = value.to_string().ok();
                    windows::Win32::System::Com::CoTaskMemFree(Some(value.as_ptr().cast()));
                    result
                })
                .map(|dir| PathBuf::from(dir).join("AgentsPads"))
                .unwrap_or_else(|| PathBuf::from(r"\\.\NUL\AgentsPads"))
        }
    }
    #[cfg(not(windows))]
    {
        platform_user_dir()
    }
}

fn platform_user_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home().join("Library/Application Support/AgentsPads")
    }
    #[cfg(windows)]
    {
        PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into())).join("AgentsPads")
    }
}

/// 提权时只用受保护目录，环境变量和用户目录都不参与。未提权时环境变量优先。
fn select_runtime_dir(
    env_override: Option<&str>,
    elevated: bool,
    protected_state: Option<&Path>,
    user_dir: &Path,
) -> Option<PathBuf> {
    if elevated {
        return protected_state.map(Path::to_path_buf);
    }
    match env_override.map(str::trim).filter(|path| !path.is_empty()) {
        Some(path) => Some(PathBuf::from(path)),
        None => Some(user_dir.to_path_buf()),
    }
}

pub fn log_dir() -> PathBuf {
    writable_log_dir().unwrap_or_else(|| {
        #[cfg(windows)]
        {
            unwritable_elevated_dir().join("logs")
        }
        #[cfg(not(windows))]
        {
            home().join("Library/Logs/AgentsPads")
        }
    })
}

fn writable_log_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        select_log_dir(true, runtime_dir().as_deref(), Path::new(""))
    }
    #[cfg(not(windows))]
    {
        select_log_dir(false, None, &home().join("Library/Logs/AgentsPads"))
    }
}

fn select_log_dir(
    windows_layout: bool,
    runtime: Option<&Path>,
    macos_logs: &Path,
) -> Option<PathBuf> {
    if windows_layout {
        runtime.map(|dir| dir.join("logs"))
    } else {
        Some(macos_logs.to_path_buf())
    }
}

#[cfg(not(windows))]
fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
}

/// 管理员实例的密钥不能放在普通进程可读的用户目录，否则同用户的普通程序读到密钥后
/// 就能借管理员实例向高权限窗口注入输入。所以按进程实际是否提权选存放位置：
/// 提权时只读写 `elevation::admin_identity_path()`（仅 Administrators/SYSTEM 可访问），
/// 失败即报错，绝不回退到普通实例的 identity.json。
#[cfg(windows)]
fn elevated() -> bool {
    crate::elevation::is_elevated()
}

pub fn load() -> std::io::Result<Identity> {
    #[cfg(windows)]
    if elevated() {
        return load_admin();
    }
    Ok(load_normal_at(&data_dir().join("identity.json")))
}

pub fn save(id: &Identity) -> std::io::Result<()> {
    #[cfg(windows)]
    if elevated() {
        return write_json(&crate::elevation::admin_identity_path()?, id);
    }
    write_json(&data_dir().join("identity.json"), id)
}

fn read_normal_at(path: &Path) -> Option<Identity> {
    let text = read_capped(path, 4096)?;
    serde_json::from_str::<Identity>(&text)
        .ok()
        .filter(|id| !id.device_id.is_empty())
}

fn load_normal_at(path: &Path) -> Identity {
    if let Some(mut id) = read_normal_at(path) {
        if id.secret.is_empty() {
            id.secret = crate::pairing::random_hex(32);
            let _ = write_json(path, &id);
        }
        return id;
    }
    let id = seed(None);
    let _ = write_json(path, &id);
    id
}

/// 保留普通实例的 device_id 与展示名（手机端仍是同一台电脑），只换新密钥。
fn seed(base: Option<Identity>) -> Identity {
    let base = base.unwrap_or_else(|| Identity {
        device_id: uuid::Uuid::new_v4().to_string(),
        name: whoami::fallible::hostname().unwrap_or_else(|_| "AgentsPads".into()),
        secret: String::new(),
    });
    Identity {
        secret: crate::pairing::random_hex(32),
        ..base
    }
}

#[cfg(windows)]
fn load_admin() -> std::io::Result<Identity> {
    let path = crate::elevation::admin_identity_path()?;
    let saved = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Identity>(&s).ok())
        .filter(|id| !id.device_id.is_empty() && id.secret.len() == 64);
    if let Some(id) = saved {
        return Ok(id);
    }
    // 首次进入管理员模式：新密钥，手机需重新扫码。用户文件只提供过滤后的 device_id 与展示名。
    let id =
        seed(read_normal_at(&fixed_user_data_dir().join("identity.json")).map(admin_seed_base));
    write_json(&path, &id)?;
    Ok(id)
}

fn write_json(path: &std::path::Path, id: &Identity) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(id)?)?;
        file.sync_all()?;
        drop(file);
        #[cfg(windows)]
        {
            use windows::{
                core::HSTRING,
                Win32::Storage::FileSystem::{
                    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
                },
            };
            unsafe {
                MoveFileExW(
                    &HSTRING::from(temporary.as_os_str()),
                    &HSTRING::from(path.as_os_str()),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            }
            .map_err(std::io::Error::other)?;
        }
        #[cfg(not(windows))]
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// 用户目录里的身份只接受 UUID 和干净的展示名，密钥一律丢掉。
#[cfg(any(windows, test))]
fn admin_seed_base(id: Identity) -> Identity {
    let device_id = if uuid::Uuid::parse_str(id.device_id.trim()).is_ok() {
        id.device_id.trim().to_string()
    } else {
        uuid::Uuid::new_v4().to_string()
    };
    Identity {
        device_id,
        name: sanitize_display_name(&id.name),
        secret: String::new(),
    }
}

#[cfg(any(windows, test))]
fn sanitize_display_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        whoami::fallible::hostname().unwrap_or_else(|_| "AgentsPads".into())
    } else {
        name.to_string()
    }
}

fn read_capped(path: &Path, max: usize) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > max as u64 {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut buf).ok()?;
    if buf.len() > max {
        return None;
    }
    String::from_utf8(buf).ok()
}

fn resolve_theme(primary: Option<&str>, elevated: bool, user_theme: Option<&str>) -> String {
    fn accept(text: Option<&str>) -> Option<String> {
        let text = text?.trim();
        if text == "light" || text == "dark" || text == "system" {
            Some(text.to_string())
        } else {
            None
        }
    }
    accept(primary)
        .or_else(|| if elevated { accept(user_theme) } else { None })
        .unwrap_or_else(|| "system".into())
}

pub fn load_theme() -> String {
    let elevated = crate::elevation::is_elevated();
    let primary = runtime_dir().and_then(|dir| read_theme_at(&dir));
    let user = if elevated {
        read_theme_at(&fixed_user_data_dir())
    } else {
        None
    };
    resolve_theme(primary.as_deref(), elevated, user.as_deref())
}

pub fn save_theme(theme: &str) {
    let Some(dir) = runtime_dir() else {
        return;
    };
    let _ = save_theme_at(&dir, theme);
}

fn read_theme_at(dir: &Path) -> Option<String> {
    read_capped(&dir.join("theme.txt"), 32)
}

fn save_theme_at(dir: &Path, theme: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("theme.txt"), theme)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_device_id() {
        let dir = std::env::temp_dir().join(format!("agentpad-id-{}", uuid::Uuid::new_v4()));
        let path = dir.join("identity.json");
        let a = load_normal_at(&path);
        let b = load_normal_at(&path);
        assert_eq!(a.device_id, b.device_id);
        assert!(!a.device_id.is_empty());
        assert_eq!(a.secret, b.secret);
        assert_eq!(a.secret.len(), 64);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn persists_repaired_secret_without_changing_device_id() {
        let dir = std::env::temp_dir().join(format!("agentpad-id-{}", uuid::Uuid::new_v4()));
        let path = dir.join("identity.json");
        let original = Identity {
            device_id: uuid::Uuid::new_v4().to_string(),
            name: "Desk".into(),
            secret: String::new(),
        };
        write_json(&path, &original).unwrap();
        let repaired = load_normal_at(&path);
        assert_eq!(repaired.device_id, original.device_id);
        assert_eq!(repaired.name, original.name);
        assert_eq!(repaired.secret.len(), 64);
        assert_eq!(load_normal_at(&path).secret, repaired.secret);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn persists_theme_and_ignores_invalid_values() {
        let dir = std::env::temp_dir().join(format!("agentpad-theme-{}", uuid::Uuid::new_v4()));
        save_theme_at(&dir, "dark").unwrap();
        assert_eq!(
            resolve_theme(read_theme_at(&dir).as_deref(), false, None),
            "dark"
        );
        save_theme_at(&dir, "nope").unwrap();
        assert_eq!(
            resolve_theme(read_theme_at(&dir).as_deref(), false, None),
            "system"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn admin_seed_keeps_device_id_but_not_secret() {
        let normal = Identity {
            device_id: "11111111-2222-4333-8444-555555555555".into(),
            name: "Desk".into(),
            secret: "a".repeat(64),
        };
        let admin = seed(Some(normal.clone()));
        assert_eq!(admin.device_id, normal.device_id);
        assert_eq!(admin.name, normal.name);
        assert_eq!(admin.secret.len(), 64);
        assert_ne!(admin.secret, normal.secret);
    }

    #[test]
    fn elevated_runtime_dir_ignores_user_controlled_paths() {
        let user = Path::new(r"C:\Users\me\AppData\Roaming\AgentsPads");
        let protected = Path::new(r"C:\Program Files\AgentsPads\state");
        let env = Some(r"D:\attacker\link");
        assert_eq!(
            select_runtime_dir(env, true, Some(protected), user).as_deref(),
            Some(protected)
        );
        assert_eq!(select_runtime_dir(env, true, None, user), None);
        let logs = select_log_dir(true, Some(protected), Path::new("unused")).unwrap();
        assert_eq!(logs, protected.join("logs"));
        assert_eq!(select_log_dir(true, None, Path::new("unused")), None);
        assert_eq!(
            select_runtime_dir(env, false, Some(protected), user).as_deref(),
            Some(Path::new(r"D:\attacker\link"))
        );
        assert_eq!(
            select_runtime_dir(Some("  "), false, None, user).as_deref(),
            Some(user)
        );
        let user_logs = select_log_dir(true, Some(user), Path::new("unused")).unwrap();
        assert_eq!(user_logs, user.join("logs"));
        let macos = Path::new("/Users/me/Library/Logs/AgentsPads");
        assert_eq!(select_log_dir(false, None, macos).as_deref(), Some(macos));
    }

    #[test]
    fn elevated_theme_falls_back_to_a_filtered_user_value() {
        assert_eq!(resolve_theme(Some("dark"), true, Some("light")), "dark");
        assert_eq!(resolve_theme(Some("nope"), true, Some(" light ")), "light");
        assert_eq!(resolve_theme(None, true, Some("../x")), "system");
        assert_eq!(resolve_theme(None, false, Some("dark")), "system");
        assert_eq!(resolve_theme(Some("system"), false, None), "system");
    }

    #[test]
    fn admin_seed_drops_unsafe_user_identity_fields() {
        let good = admin_seed_base(Identity {
            device_id: "11111111-2222-4333-8444-555555555555".into(),
            name: "  Desk  ".into(),
            secret: "c".repeat(64),
        });
        assert_eq!(good.device_id, "11111111-2222-4333-8444-555555555555");
        assert_eq!(good.name, "Desk");
        assert!(good.secret.is_empty());

        let bad = admin_seed_base(Identity {
            device_id: "../not-a-uuid".into(),
            name: "bad\nname".into(),
            secret: "b".repeat(64),
        });
        assert!(uuid::Uuid::parse_str(&bad.device_id).is_ok());
        assert_ne!(bad.device_id, "../not-a-uuid");
        assert!(!bad.name.chars().any(char::is_control));
        assert!(!bad.name.is_empty());
        assert!(bad.secret.is_empty());
    }
}
