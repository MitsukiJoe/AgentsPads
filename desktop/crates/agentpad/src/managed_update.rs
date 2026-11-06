use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const START: &str = "--managed-update-start";
const WORKER: &str = "--managed-update-worker";
const RECOVER: &str = "--managed-update-recover";
const SCHEMA: u32 = 1;
const MAX_RECORD: u64 = 16 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum Phase {
    Prepared,
    OldSaved,
    NewPlaced,
    Verifying,
    Committed,
    Restored,
    RecoveryFailed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Journal {
    schema: u32,
    transaction: String,
    version: String,
    target_hash: String,
    previous_hash: String,
    previous_transaction: Option<String>,
    elevated: bool,
    parent_pid: u32,
    parent_created: u64,
    worker_pid: u32,
    worker_created: u64,
    owner_sid: String,
    owner_session: u32,
    candidate_pid: Option<u32>,
    candidate_created: Option<u64>,
    phase: Phase,
    message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Status {
    transaction: String,
    worker_pid: u32,
    worker_created: u64,
    failed: bool,
    complete: bool,
    message: String,
}

#[derive(Clone, Debug)]
struct Request {
    transaction: String,
    version: String,
    target_hash: String,
    elevated: bool,
    parent_pid: u32,
    parent_created: u64,
}

impl Request {
    fn parse(args: &[String]) -> Result<Self, String> {
        if args.len() != 6 {
            return Err("更新参数不完整".into());
        }
        let request = Self {
            transaction: args[0].clone(),
            version: args[1].clone(),
            target_hash: args[2].to_ascii_lowercase(),
            elevated: match args[3].as_str() {
                "admin" => true,
                "normal" => false,
                _ => return Err("更新启动权限无效".into()),
            },
            parent_pid: args[4].parse().map_err(|_| "更新进程无效")?,
            parent_created: args[5].parse().map_err(|_| "更新进程时间无效")?,
        };
        if !valid_id(&request.transaction)
            || !valid_version(&request.version)
            || !valid_hash(&request.target_hash)
            || request.parent_pid == 0
            || request.parent_created == 0
        {
            return Err("更新身份参数无效".into());
        }
        Ok(request)
    }

    fn arguments(&self, action: &str) -> Vec<String> {
        vec![
            action.into(),
            self.transaction.clone(),
            self.version.clone(),
            self.target_hash.clone(),
            if self.elevated { "admin" } else { "normal" }.into(),
            self.parent_pid.to_string(),
            self.parent_created.to_string(),
        ]
    }
}

#[derive(Clone)]
struct Layout {
    executable: PathBuf,
    updates: PathBuf,
}

impl Layout {
    fn new(executable: PathBuf) -> Result<Self, String> {
        let root = executable.parent().ok_or("正式安装目录无效")?;
        let updates = root.join("updates");
        reject_reparse(&executable)?;
        reject_reparse(&updates)?;
        Ok(Self {
            executable,
            updates,
        })
    }

    fn ensure(&self) -> Result<(), String> {
        reject_reparse(&self.updates)?;
        std::fs::create_dir_all(&self.updates).map_err(|e| format!("创建更新目录失败: {e}"))
    }

    fn journal(&self) -> PathBuf {
        self.updates.join("transaction.json")
    }

    fn staged(&self, id: &str) -> PathBuf {
        self.updates.join(format!("staged-{id}.exe"))
    }

    fn backup(&self, id: &str) -> PathBuf {
        self.updates.join(format!("rollback-{id}.exe"))
    }

    fn status(&self, id: &str) -> PathBuf {
        self.updates.join(format!("status-{id}.json"))
    }

    fn worker(&self) -> PathBuf {
        self.updates.join("update-worker.exe")
    }

    fn lock(&self) -> Result<File, String> {
        self.ensure()?;
        let path = self.updates.join("update.lock");
        reject_reparse(&path)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        options
            .open(path)
            .map_err(|_| "另一个更新或恢复操作正在进行，请稍后重试".into())
    }

    fn idle(&self) -> bool {
        let path = self.updates.join("update.lock");
        if !path.exists() {
            return true;
        }
        if reject_reparse(&path).is_err() {
            return false;
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        options.open(path).is_ok()
    }

    fn load(&self) -> Result<Option<Journal>, String> {
        if !self.journal().exists() {
            return Ok(None);
        }
        let journal: Journal = read_json(&self.journal())?;
        validate_journal(&journal)?;
        Ok(Some(journal))
    }

    fn save(&self, journal: &Journal) -> Result<(), String> {
        validate_journal(journal)?;
        write_json(&self.journal(), journal)
    }

    fn publish(&self, request: &Request, message: &str, failed: bool, complete: bool) {
        let status = Status {
            transaction: request.transaction.clone(),
            worker_pid: std::process::id(),
            #[cfg(windows)]
            worker_created: crate::elevation::process_creation_time(std::process::id())
                .unwrap_or(0),
            #[cfg(not(windows))]
            worker_created: 0,
            failed,
            complete,
            message: message.into(),
        };
        let _ = write_json(&self.status(&request.transaction), &status);
    }

    fn swap(&self, journal: &mut Journal) -> Result<(), String> {
        require_hash(&self.executable, &journal.previous_hash)?;
        require_hash(&self.staged(&journal.transaction), &journal.target_hash)?;
        let backup = self.backup(&journal.transaction);
        if backup.exists() {
            return Err("本次更新的备份位置已存在，已停止替换".into());
        }
        journal.phase = Phase::Prepared;
        self.save(journal)?;
        replace_with_backup(
            &self.executable,
            &self.staged(&journal.transaction),
            &backup,
        )?;
        journal.phase = Phase::NewPlaced;
        self.save(journal)?;
        require_hash(&backup, &journal.previous_hash)?;
        require_hash(&self.executable, &journal.target_hash)
    }

    fn restore_files(&self, journal: &mut Journal) -> Result<(), String> {
        if hash_file(&self.executable).ok().as_deref() == Some(&journal.previous_hash) {
            return Ok(());
        }
        let backup = self.backup(&journal.transaction);
        require_hash(&backup, &journal.previous_hash)
            .map_err(|e| format!("旧版本备份不可用，已保留现场供修复: {e}"))?;
        if self.executable.exists() {
            require_hash(&self.executable, &journal.target_hash)
                .map_err(|_| "正式程序已被其他操作修改，已停止自动恢复并保留备份".to_string())?;
            let failed = self
                .updates
                .join(format!("failed-{}.exe", journal.transaction));
            if failed.exists() {
                return Err("失败版本保存位置已存在，已保留全部文件供修复".into());
            }
            if let Err(error) = replace_with_backup(&self.executable, &backup, &failed) {
                if hash_file(&self.executable).ok().as_deref() == Some(&journal.previous_hash) {
                    return Ok(());
                }
                if self.executable.exists() {
                    return Err(format!("恢复文件失败，旧版本仍保留: {error}"));
                }
                require_hash(&backup, &journal.previous_hash)?;
                std::fs::rename(&backup, &self.executable)
                    .map_err(|e| format!("旧版本仍在备份位置，恢复失败: {e}"))?;
            }
        } else {
            std::fs::rename(&backup, &self.executable)
                .map_err(|e| format!("旧版本仍在备份位置，恢复失败: {e}"))?;
        }
        require_hash(&self.executable, &journal.previous_hash)
    }

    fn commit(&self, journal: &mut Journal) -> Result<(), String> {
        require_hash(&self.executable, &journal.target_hash)?;
        require_hash(&self.backup(&journal.transaction), &journal.previous_hash)?;
        journal.phase = Phase::Committed;
        journal.message = format!("已更新至 {}，新版已通过启动检查", journal.version);
        self.save(journal)?;
        if let Some(previous) = &journal.previous_transaction {
            if previous != &journal.transaction {
                let _ = std::fs::remove_file(self.backup(previous));
                let _ = std::fs::remove_file(self.status(previous));
            }
        }
        Ok(())
    }
}

fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|parsed| parsed.to_string() == id)
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

fn valid_version(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u64>().is_ok()
        })
}

fn validate_journal(journal: &Journal) -> Result<(), String> {
    if journal.schema != SCHEMA
        || !valid_id(&journal.transaction)
        || !valid_version(&journal.version)
        || !valid_hash(&journal.target_hash)
        || !valid_hash(&journal.previous_hash)
        || journal.parent_pid == 0
        || journal.parent_created == 0
        || journal.worker_pid == 0
        || journal.worker_created == 0
        || !journal.owner_sid.starts_with("S-1-")
        || journal.owner_sid.len() > 184
        || !journal.owner_sid[4..]
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'-')
        || journal.candidate_pid.is_some() != journal.candidate_created.is_some()
        || journal.candidate_pid == Some(0)
        || journal.candidate_created == Some(0)
        || journal
            .previous_transaction
            .as_ref()
            .is_some_and(|id| !valid_id(id))
    {
        Err("更新恢复记录无效；请运行安装包修复，已有程序和备份均未清理".into())
    } else {
        Ok(())
    }
}

fn reject_reparse(path: &Path) -> Result<(), String> {
    for part in path.ancestors() {
        match std::fs::symlink_metadata(part) {
            Ok(meta) => {
                let linked = meta.file_type().is_symlink();
                #[cfg(windows)]
                let linked = {
                    use std::os::windows::fs::MetadataExt;
                    linked || meta.file_attributes() & 0x400 != 0
                };
                if linked {
                    return Err("安装或更新路径包含重定向，已停止操作".into());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("无法验证安装路径: {e}")),
        }
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<String, String> {
    reject_reparse(path)?;
    let mut file = File::open(path).map_err(|e| format!("读取程序文件失败: {e}"))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|e| format!("校验程序文件失败: {e}"))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn require_hash(path: &Path, expected: &str) -> Result<(), String> {
    if hash_file(path)?.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err("程序文件与本次选择的构建不一致".into())
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    reject_reparse(path)?;
    let file = File::open(path).map_err(|e| format!("读取更新状态失败: {e}"))?;
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_RECORD {
        return Err("更新状态文件过大".into());
    }
    serde_json::from_reader(file.take(MAX_RECORD)).map_err(|e| format!("更新状态损坏: {e}"))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    reject_reparse(path)?;
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| format!("保存更新状态失败: {e}"))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("保存更新状态失败: {e}"))?;
    drop(file);
    let result = replace_path(&temp, path);
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn replace_path(source: &Path, destination: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::FileSystem::{ReplaceFileW, REPLACE_FILE_FLAGS};
        if destination.exists() {
            let source = windows::core::HSTRING::from(source.as_os_str());
            let destination = windows::core::HSTRING::from(destination.as_os_str());
            return unsafe {
                ReplaceFileW(
                    &destination,
                    &source,
                    None,
                    REPLACE_FILE_FLAGS(0),
                    None,
                    None,
                )
            }
            .map_err(|e| format!("提交更新状态失败: {e}"));
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
        }
        let source: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<_> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 1 | 8) } == 0 {
            Err(format!(
                "提交更新状态失败: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(())
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(source, destination).map_err(|e| e.to_string())
    }
}

fn replace_with_backup(current: &Path, replacement: &Path, backup: &Path) -> Result<(), String> {
    reject_reparse(current)?;
    reject_reparse(replacement)?;
    reject_reparse(backup)?;
    #[cfg(windows)]
    {
        use windows::Win32::Storage::FileSystem::{ReplaceFileW, REPLACE_FILE_FLAGS};
        let current = windows::core::HSTRING::from(current.as_os_str());
        let replacement = windows::core::HSTRING::from(replacement.as_os_str());
        let backup = windows::core::HSTRING::from(backup.as_os_str());
        unsafe {
            ReplaceFileW(
                &current,
                &replacement,
                &backup,
                REPLACE_FILE_FLAGS(0),
                None,
                None,
            )
        }
        .map_err(|e| format!("Windows 未完成文件替换，将检查现有文件并恢复: {e}"))
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(current, backup)
            .and_then(|_| std::fs::rename(replacement, current))
            .map_err(|e| e.to_string())
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows::Win32::Foundation::{ERROR_NO_MORE_FILES, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessId, OpenProcess, TerminateProcess, WaitForSingleObject,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    fn ensure_no_other_instances(layout: &Layout, parent_pid: u32) -> Result<(), String> {
        use windows::Win32::System::Threading::{QueryFullProcessImageNameW, PROCESS_NAME_WIN32};
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
            .map_err(|e| format!("无法检查其他运行实例: {e}"))?;
        let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot.0) };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let name = layout
            .executable
            .file_name()
            .ok_or("正式程序文件名无效")?
            .to_string_lossy();
        let mut next = unsafe { Process32FirstW(HANDLE(snapshot.as_raw_handle()), &mut entry) };
        loop {
            match next {
                Ok(()) => {}
                Err(error)
                    if error.code()
                        == windows::core::HRESULT::from_win32(ERROR_NO_MORE_FILES.0) =>
                {
                    break
                }
                Err(error) => return Err(format!("无法完整检查运行实例: {error}")),
            }
            let length = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let process_name = String::from_utf16_lossy(&entry.szExeFile[..length]);
            if entry.th32ProcessID != parent_pid
                && entry.th32ProcessID != std::process::id()
                && process_name.eq_ignore_ascii_case(&name)
            {
                let handle = unsafe {
                    OpenProcess(
                        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                        false,
                        entry.th32ProcessID,
                    )
                }
                .map_err(|_| {
                    "另一个账户或会话中可能有 AgentsPads 正在运行，请关闭后再更新".to_string()
                })?;
                let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
                if !wait(&handle, Duration::ZERO) {
                    let mut buffer = [0u16; 32768];
                    let mut size = buffer.len() as u32;
                    unsafe {
                        QueryFullProcessImageNameW(
                            HANDLE(handle.as_raw_handle()),
                            PROCESS_NAME_WIN32,
                            windows::core::PWSTR(buffer.as_mut_ptr()),
                            &mut size,
                        )
                    }
                    .map_err(|_| {
                        "无法验证另一个 AgentsPads 实例，请先关闭其他账户或会话中的程序".to_string()
                    })?;
                    let path = PathBuf::from(String::from_utf16_lossy(&buffer[..size as usize]));
                    if same_path(&path, &layout.executable) {
                        return Err("另一个账户或会话中的 AgentsPads 仍在运行；请先关闭它，再更新同一份正式程序".into());
                    }
                }
            }
            next = unsafe { Process32NextW(HANDLE(snapshot.as_raw_handle()), &mut entry) };
        }
        Ok(())
    }

    pub(super) fn layout() -> Result<Layout, String> {
        let layout = Layout::new(crate::elevation::validate_install()?)?;
        match std::fs::symlink_metadata(&layout.updates) {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    return Err("更新目录类型无效，请运行安装包修复".into());
                }
                reject_reparse(&layout.updates)?;
                crate::managed_tasks::validate_install_acl(&layout.updates)?;
                for entry in std::fs::read_dir(&layout.updates)
                    .map_err(|e| format!("无法检查更新目录: {e}"))?
                {
                    let path = entry.map_err(|e| format!("无法检查更新文件: {e}"))?.path();
                    reject_reparse(&path)?;
                    let metadata = std::fs::symlink_metadata(&path)
                        .map_err(|e| format!("无法验证更新文件类型: {e}"))?;
                    if !metadata.is_file() {
                        return Err("更新目录包含非预期目录或文件类型，请运行安装包修复".into());
                    }
                    crate::managed_tasks::validate_install_acl(&path)?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("无法验证更新目录: {error}")),
        }
        Ok(layout)
    }

    fn same_path(a: &Path, b: &Path) -> bool {
        matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
    }

    fn wait(handle: &OwnedHandle, timeout: Duration) -> bool {
        unsafe {
            WaitForSingleObject(
                HANDLE(handle.as_raw_handle()),
                timeout.as_millis().min(u32::MAX as u128) as u32,
            ) == WAIT_OBJECT_0
        }
    }

    fn open_process(pid: u32, created: u64) -> Result<OwnedHandle, String> {
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                pid,
            )
        }
        .map_err(|e| format!("无法验证待更新进程: {e}"))?;
        let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        if crate::elevation::process_creation_time(pid)? != created {
            return Err("待更新进程已变化，已停止更新".into());
        }
        Ok(handle)
    }

    pub(super) fn process_running(pid: u32, created: u64) -> bool {
        open_process(pid, created).is_ok_and(|process| !wait(&process, Duration::ZERO))
    }

    fn session() -> Result<u32, String> {
        let mut session = 0;
        unsafe {
            windows::Win32::System::RemoteDesktop::ProcessIdToSessionId(
                std::process::id(),
                &mut session,
            )
        }
        .map_err(|e| e.to_string())?;
        Ok(session)
    }

    fn validate_recovery_owner(journal: &Journal) -> Result<(), String> {
        if journal.owner_sid != crate::managed_tasks::current_user_sid()? {
            return Err("上次更新由另一个 Windows 账户发起；请使用原账户完成恢复，或运行安装包修复。已有程序和备份均未修改".into());
        }
        if journal.owner_session != session()?
            && (process_running(journal.parent_pid, journal.parent_created)
                || process_running(journal.worker_pid, journal.worker_created)
                || journal
                    .candidate_pid
                    .zip(journal.candidate_created)
                    .is_some_and(|(pid, created)| process_running(pid, created)))
        {
            return Err(
                "原登录会话仍有更新相关实例在运行，请回到该会话完成操作，或先关闭它再恢复".into(),
            );
        }

        Ok(())
    }

    pub(super) fn begin(
        info: &crate::updater::UpdateInfo,
        status: &Arc<Mutex<crate::updater::UpdateStatus>>,
    ) -> Result<(), String> {
        let layout = layout()?;
        let current = std::env::current_exe().map_err(|e| e.to_string())?;
        if !same_path(&current, &layout.executable) {
            return Err(
                "请从已安装的 AgentsPads 检查更新；下载的文件不会替换其他位置的程序".into(),
            );
        }
        let request = Request {
            transaction: uuid::Uuid::new_v4().to_string(),
            version: info.version.clone(),
            target_hash: info.sha256.to_ascii_lowercase(),
            elevated: crate::elevation::is_elevated(),
            parent_pid: std::process::id(),
            parent_created: crate::elevation::process_creation_time(std::process::id())?,
        };
        Request::parse(&request.arguments(START)[1..])?;
        *status.lock().unwrap() = crate::updater::UpdateStatus::Updating(
            "正在准备更新；普通模式需要一次管理员授权…".into(),
        );
        let child = crate::elevation::launch_same_user(
            &layout.executable,
            &request.arguments(START),
            true,
        )?;
        let started = Instant::now();
        let mut last_running = Instant::now();
        while started.elapsed() < Duration::from_secs(20 * 60) {
            if let Ok(record) = read_json::<Status>(&layout.status(&request.transaction)) {
                if record.transaction != request.transaction {
                    return Err("更新状态与本次操作不一致".into());
                }
                if record.failed {
                    return Err(record.message);
                }
                if record.complete {
                    return Ok(());
                }
                if open_process(record.worker_pid, record.worker_created)
                    .is_ok_and(|worker| !wait(&worker, Duration::ZERO))
                {
                    last_running = Instant::now();
                }
                *status.lock().unwrap() = crate::updater::UpdateStatus::Updating(record.message);
            }
            if last_running.elapsed() > Duration::from_secs(10) && wait(&child, Duration::ZERO) {
                let mut code = 1;
                let _ = unsafe { GetExitCodeProcess(HANDLE(child.as_raw_handle()), &mut code) };
                return Err(format!("更新助手已停止（状态 {code}）；当前版本仍在运行。请重启程序恢复或运行安装包修复"));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Err("更新未完成；当前程序仍在运行，请检查更新状态后再试".into())
    }

    pub(super) fn dispatch(action: &str, args: &[String]) -> Result<(), String> {
        if !crate::elevation::is_elevated() {
            return Err("更新和修复安装需要管理员授权".into());
        }
        crate::managed_tasks::validate_desktop_user()?;
        let layout = layout()?;
        layout.ensure()?;
        let current = std::env::current_exe().map_err(|e| e.to_string())?;
        if action == RECOVER {
            if !args.is_empty() || !same_path(&current, &layout.worker()) {
                return Err("恢复入口无效，请运行安装包修复".into());
            }
            let _operation = crate::elevation::operation_lock()?;
            let _lock = layout.lock()?;
            let mut journal = layout.load()?.ok_or("没有待恢复的更新")?;
            if journal.phase == Phase::Committed || journal.phase == Phase::Restored {
                return Ok(());
            }
            validate_recovery_owner(&journal)?;
            require_hash(&current, &journal.previous_hash)?;
            stop_recorded_candidate(&journal)?;
            let parent_running = open_process(journal.parent_pid, journal.parent_created)
                .is_ok_and(|parent| !wait(&parent, Duration::ZERO));
            return recover(&layout, &mut journal, "上次更新被中断", !parent_running);
        }
        let request = Request::parse(args)?;
        let result = if action == START {
            prepare_worker(&layout, &current, &request)
        } else if action == WORKER {
            if !same_path(&current, &layout.worker()) {
                return Err("更新助手位置无效".into());
            }
            run_worker(&layout, &request)
        } else {
            Err("未知更新操作".into())
        };
        if let Err(error) = &result {
            layout.publish(&request, error, true, false);
        }
        result
    }

    fn prepare_worker(layout: &Layout, current: &Path, request: &Request) -> Result<(), String> {
        if !same_path(current, &layout.executable) {
            return Err("更新入口必须是正式安装的程序".into());
        }
        let _operation = crate::elevation::operation_lock()?;
        let _lock = layout.lock()?;
        if layout
            .load()?
            .is_some_and(|j| !matches!(j.phase, Phase::Committed | Phase::Restored))
        {
            return Err("上次更新尚未恢复，请先重新启动程序或运行安装包修复".into());
        }
        let _parent = open_process(request.parent_pid, request.parent_created)?;
        crate::elevation::validate_process(request.parent_pid, request.elevated)?;
        layout.publish(request, "正在启动更新助手…", false, false);
        let worker_next = layout
            .updates
            .join(format!("worker-{}.tmp", request.transaction));
        std::fs::copy(&layout.executable, &worker_next)
            .map_err(|e| format!("准备独立更新助手失败: {e}"))?;
        require_hash(&worker_next, &hash_file(&layout.executable)?)?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&worker_next)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        reject_reparse(&layout.worker())?;
        replace_path(&worker_next, &layout.worker())?;
        let _worker =
            crate::elevation::launch_same_user(&layout.worker(), &request.arguments(WORKER), true)?;
        Ok(())
    }

    fn run_worker(layout: &Layout, request: &Request) -> Result<(), String> {
        run_worker_with_download(layout, request, crate::updater::download_managed_release)
    }

    fn run_worker_with_download(
        layout: &Layout,
        request: &Request,
        download_release: impl FnOnce(&str, &str, &Path, &dyn Fn(u64, u64)) -> Result<(), String>,
    ) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let _operation = loop {
            match crate::elevation::operation_lock() {
                Ok(lock) => break lock,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100))
                }
                Err(error) => return Err(error),
            }
        };
        let _lock = layout.lock()?;
        let previous = layout.load()?;
        if previous
            .as_ref()
            .is_some_and(|j| !matches!(j.phase, Phase::Committed | Phase::Restored))
        {
            return Err("已有未完成的更新，已停止新操作".into());
        }
        let parent = open_process(request.parent_pid, request.parent_created)?;
        crate::elevation::validate_process(request.parent_pid, request.elevated)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match ensure_no_other_instances(layout, request.parent_pid) {
                Ok(()) => break,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100))
                }
                Err(error) => return Err(error),
            }
        }
        let old_hash = hash_file(&layout.executable)?;
        require_hash(&layout.worker(), &old_hash)?;
        let staged = layout.staged(&request.transaction);
        reject_reparse(&staged)?;
        if staged.exists() {
            return Err("本次下载暂存位置已存在，已停止更新".into());
        }
        layout.publish(request, "正在下载并验证正式安装的新版本…", false, false);
        let last_progress = std::cell::Cell::new(Instant::now());
        let download = download_release(
            &request.version,
            &request.target_hash,
            &staged,
            &|done, total| {
                if last_progress.get().elapsed() < Duration::from_millis(250) && done != total {
                    return;
                }
                last_progress.set(Instant::now());
                let message = if total > 0 {
                    format!("正在下载更新… {}%", done * 100 / total.max(1))
                } else {
                    format!("正在下载更新… {:.1} MB", done as f64 / 1048576.0)
                };
                layout.publish(request, &message, false, false);
            },
        );
        if let Err(error) = download {
            let _ = std::fs::remove_file(&staged);
            return Err(error);
        }
        require_hash(&staged, &request.target_hash)?;
        ensure_no_other_instances(layout, request.parent_pid)?;
        let mut journal = Journal {
            schema: SCHEMA,
            transaction: request.transaction.clone(),
            version: request.version.clone(),
            target_hash: request.target_hash.clone(),
            previous_hash: old_hash,
            previous_transaction: previous.and_then(|j| {
                if j.phase == Phase::Committed {
                    Some(j.transaction)
                } else {
                    j.previous_transaction
                }
            }),
            elevated: request.elevated,
            parent_pid: request.parent_pid,
            parent_created: request.parent_created,
            worker_pid: std::process::id(),
            worker_created: crate::elevation::process_creation_time(std::process::id())?,
            owner_sid: crate::managed_tasks::current_user_sid()?,
            owner_session: session()?,
            candidate_pid: None,
            candidate_created: None,
            phase: Phase::Prepared,
            message: "新版本已准备，尚未替换当前程序".into(),
        };
        layout.save(&journal)?;
        layout.publish(
            request,
            "新版本已验证，正在关闭当前程序并保留备份…",
            false,
            false,
        );
        if wait(&parent, Duration::ZERO) {
            recover(layout, &mut journal, "原实例已提前退出，已取消更新", false)?;
            return Err(journal.message);
        }
        if let Err(error) = crate::elevation::request_close(request.parent_pid) {
            recover(layout, &mut journal, &error, false)?;
            return Err(journal.message);
        }
        if !wait(&parent, Duration::from_secs(30)) {
            recover(
                layout,
                &mut journal,
                "当前程序没有退出，已取消替换；请先关闭程序后重试",
                false,
            )?;
            return Err(journal.message);
        }
        let result: Result<(), String> = (|| {
            layout.swap(&mut journal)?;
            layout.publish(
                request,
                "正在启动并检查新版的版本、权限和连接服务…",
                false,
                false,
            );
            let candidate = launch_candidate(layout, &mut journal)?;
            if !wait_ready(
                &candidate,
                &journal.target_hash,
                journal.elevated,
                &journal.transaction,
            ) {
                stop_child(&candidate)?;
                return Err("新版未通过启动检查".into());
            }
            layout.commit(&mut journal)?;
            layout.publish(request, &journal.message, false, true);
            Ok(())
        })();
        if let Err(error) = result {
            if journal.phase == Phase::RecoveryFailed {
                return Err(journal.message);
            }
            if let Err(stop_error) = stop_recorded_candidate(&journal) {
                journal.phase = Phase::RecoveryFailed;
                journal.message = format!("更新未完成：{error}；{stop_error}。旧版本备份仍保留，请关闭程序后运行安装包修复");
                let _ = layout.save(&journal);
                return Err(journal.message);
            }
            recover(layout, &mut journal, &error, true)?;
            return Err(journal.message);
        }
        Ok(())
    }

    fn launch_candidate(layout: &Layout, journal: &mut Journal) -> Result<OwnedHandle, String> {
        let mut args = vec!["--ready-challenge".into(), journal.transaction.clone()];
        if journal.elevated {
            args.push("--elevated".into());
        }
        journal.phase = Phase::Verifying;
        layout.save(journal)?;
        let child =
            crate::elevation::launch_same_user(&layout.executable, &args, journal.elevated)?;
        let pid = unsafe { GetProcessId(HANDLE(child.as_raw_handle())) };
        journal.candidate_pid = Some(pid);
        let record_result = (|| {
            journal.candidate_created = Some(crate::elevation::process_creation_time(pid)?);
            layout.save(journal)
        })();
        if let Err(error) = record_result {
            if let Err(stop_error) = stop_child(&child) {
                journal.phase = Phase::RecoveryFailed;
                if journal.candidate_created.is_none() {
                    journal.candidate_pid = None;
                }
                journal.message = format!("无法保存新版启动状态：{error}；{stop_error}。已保留现有程序和恢复文件，请关闭程序后运行安装包修复");
                let _ = layout.save(journal);
                return Err(journal.message.clone());
            }
            return Err(error);
        }
        Ok(child)
    }

    fn wait_ready(child: &OwnedHandle, hash: &str, elevated: bool, challenge: &str) -> bool {
        let pid = unsafe { GetProcessId(HANDLE(child.as_raw_handle())) };
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if wait(child, Duration::ZERO) {
                return false;
            }
            if crate::elevation::verify_ready(pid, hash, elevated, challenge) {
                return !wait(child, Duration::from_secs(2))
                    && crate::elevation::verify_ready(pid, hash, elevated, challenge);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    fn stop_child(child: &OwnedHandle) -> Result<(), String> {
        if wait(child, Duration::ZERO) {
            return Ok(());
        }
        let pid = unsafe { GetProcessId(HANDLE(child.as_raw_handle())) };
        let _ = crate::elevation::request_close(pid);
        if wait(child, Duration::from_secs(5)) {
            return Ok(());
        }
        unsafe { TerminateProcess(HANDLE(child.as_raw_handle()), 1) }
            .map_err(|_| "失败的新实例无法退出，已保留旧版备份；请关闭它后修复".to_string())?;
        if wait(child, Duration::from_secs(5)) {
            Ok(())
        } else {
            Err("失败的新实例仍在运行，已停止恢复".into())
        }
    }

    fn stop_recorded_candidate(journal: &Journal) -> Result<(), String> {
        let (Some(pid), Some(created)) = (journal.candidate_pid, journal.candidate_created) else {
            return Ok(());
        };
        if crate::elevation::process_creation_time(pid).ok() != Some(created) {
            return Ok(());
        }
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
                false,
                pid,
            )
        }
        .map_err(|_| "无法检查上次新实例，请关闭它后重试".to_string())?;
        let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        if crate::elevation::process_creation_time(pid).ok() != Some(created) {
            return Ok(());
        }
        if wait(&handle, Duration::ZERO) {
            return Ok(());
        }
        if journal.owner_sid != crate::managed_tasks::current_user_sid()?
            || journal.owner_session != session()?
            || journal.owner_sid != crate::managed_tasks::process_user_sid(pid)?
        {
            return Err("待恢复实例的账户或会话与本次更新不匹配，已停止关闭操作".into());
        }
        crate::elevation::validate_process(pid, journal.elevated)?;
        stop_child(&handle)
    }

    fn recover(
        layout: &Layout,
        journal: &mut Journal,
        reason: &str,
        restart: bool,
    ) -> Result<(), String> {
        let result: Result<(), String> = (|| {
            layout.restore_files(journal)?;
            journal.phase = Phase::Restored;
            journal.candidate_pid = None;
            journal.candidate_created = None;
            journal.message = format!("更新未完成：{reason}。旧版本文件已恢复");
            layout.save(journal)?;
            if restart {
                let mut args = vec!["--ready-challenge".into(), journal.transaction.clone()];
                if journal.elevated {
                    args.push("--elevated".into());
                }
                let child = crate::elevation::launch_same_user(
                    &layout.executable,
                    &args,
                    journal.elevated,
                )?;
                if !wait_ready(
                    &child,
                    &journal.previous_hash,
                    journal.elevated,
                    &journal.transaction,
                ) {
                    let _ = stop_child(&child);
                    return Err("旧版本文件已恢复，但未通过启动检查；请运行安装包修复".into());
                }
                journal.message = format!("更新未完成：{reason}。已恢复并启动上一可用版本");
                layout.save(journal)?;
            }
            Ok(())
        })();
        if let Err(error) = &result {
            journal.phase = Phase::RecoveryFailed;
            journal.message =
                format!("更新失败：{reason}。恢复尚未完成：{error}。已保留现有程序和恢复文件，请运行安装包修复");
            let _ = layout.save(journal);
            return Err(journal.message.clone());
        }
        Ok(())
    }

    pub(super) fn recover_before_start() -> Result<(), String> {
        let current = std::env::current_exe().map_err(|e| e.to_string())?;
        if !same_path(&current, &crate::elevation::installed_executable()?) {
            return Ok(());
        }
        let layout = layout()?;
        let loaded = layout.load()?;
        let args: Vec<_> = std::env::args().collect();
        let challenge = args
            .windows(2)
            .find(|p| p[0] == "--ready-challenge")
            .map(|p| p[1].as_str());
        let expected_launch = loaded.as_ref().is_some_and(|journal| {
            matches!(journal.phase, Phase::Verifying | Phase::Restored)
                && challenge == Some(journal.transaction.as_str())
        });
        if !layout.idle() && !expected_launch {
            return Err("AgentsPads 正在更新，请等待更新完成后再启动".into());
        }
        let Some(mut journal) = loaded else {
            return Ok(());
        };
        if matches!(journal.phase, Phase::Committed | Phase::Restored) {
            return Ok(());
        }
        validate_recovery_owner(&journal)?;
        if journal.phase == Phase::Verifying && challenge == Some(journal.transaction.as_str()) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while journal.candidate_pid.is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
                journal = layout.load()?.ok_or("更新启动记录丢失")?;
            }
            if journal.candidate_pid != Some(std::process::id())
                || journal.candidate_created
                    != Some(crate::elevation::process_creation_time(std::process::id())?)
            {
                return Err("更新助手未确认本次启动，程序已停止；请重新打开以恢复旧版本".into());
            }
            require_hash(&layout.executable, &journal.target_hash)?;
            return Ok(());
        }
        if journal.phase == Phase::RecoveryFailed {
            return Err(journal.message);
        }
        if !layout.idle() {
            return Err("AgentsPads 正在更新，请等待更新完成后再启动".into());
        }
        require_hash(&layout.worker(), &journal.previous_hash)?;
        let helper = crate::elevation::launch_same_user(&layout.worker(), &[RECOVER.into()], true)?;
        if !wait(&helper, Duration::from_secs(70)) {
            return Err("等待更新恢复超时；请检查程序状态或运行安装包修复".into());
        }
        let recovered = layout.load()?.ok_or("更新恢复状态丢失")?;
        if recovered.phase == Phase::Restored {
            std::process::exit(0);
        } else {
            Err(if recovered.message.is_empty() {
                "恢复助手未完成操作；程序及恢复文件已保留，请检查系统提示或使用安装包修复。".into()
            } else {
                recovered.message
            })
        }
    }

    #[cfg(test)]
    mod live_runtime_tests {
        use super::*;
        use std::sync::atomic::{AtomicBool, Ordering};

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Plan {
            confirm_live_update: bool,
            installed_directory: PathBuf,
            parent_pid: u32,
            parent_created: u64,
            elevated: bool,
            baseline_fixture: PathBuf,
            baseline_version: String,
            baseline_hash: String,
            candidate_fixture: PathBuf,
            candidate_version: String,
            candidate_hash: String,
            expect_success: bool,
            evidence_path: PathBuf,
        }

        struct Attempt {
            result: Result<(), String>,
            journal: Option<Journal>,
            evidence: serde_json::Value,
        }

        fn ready_evidence(
            layout: &Layout,
            hash: &str,
            elevated: bool,
        ) -> Result<serde_json::Value, String> {
            let directory = if elevated {
                layout
                    .executable
                    .parent()
                    .ok_or("missing installation directory")?
                    .join("state")
                    .join(crate::managed_tasks::current_user_sid()?)
            } else {
                crate::identity::fixed_user_data_dir().join("runtime")
            };
            let record: serde_json::Value =
                read_json(&directory.join(format!("ready-{}.json", session()?)))?;
            let pid = record["pid"]
                .as_u64()
                .and_then(|pid| u32::try_from(pid).ok())
                .ok_or("readiness record lacks a valid process")?;
            let challenge = record["challenge"].as_str().ok_or("missing challenge")?;
            if !crate::elevation::verify_ready(pid, hash, elevated, challenge) {
                return Err("the real process did not pass readiness verification".into());
            }
            Ok(record)
        }

        fn prepare_worker_copy(layout: &Layout, request: &Request) -> Result<(), String> {
            let _operation = crate::elevation::operation_lock()?;
            let _lock = layout.lock()?;
            if layout
                .load()?
                .is_some_and(|j| !matches!(j.phase, Phase::Committed | Phase::Restored))
            {
                return Err("an unfinished transaction must be recovered before this test".into());
            }
            crate::elevation::validate_process(request.parent_pid, request.elevated)?;
            let next = layout
                .updates
                .join(format!("worker-{}.tmp", request.transaction));
            if next.exists() {
                return Err("test worker staging already exists".into());
            }
            std::fs::copy(&layout.executable, &next).map_err(|e| e.to_string())?;
            require_hash(&next, &hash_file(&layout.executable)?)?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&next)
                .and_then(|file| file.sync_all())
                .map_err(|e| e.to_string())?;
            replace_path(&next, &layout.worker())
        }

        fn attempt(layout: &Layout, request: &Request, fixture: &Path) -> Result<Attempt, String> {
            prepare_worker_copy(layout, request)?;
            let done = Arc::new(AtomicBool::new(false));
            let observation = Arc::new(Mutex::new(None));
            let observer = {
                let done = Arc::clone(&done);
                let observation = Arc::clone(&observation);
                let layout = layout.clone();
                let request = request.clone();
                std::thread::spawn(move || {
                    while !done.load(Ordering::Acquire) {
                        if let Ok(Some(journal)) = layout.load() {
                            if journal.transaction == request.transaction {
                                if let (Some(pid), Some(created)) =
                                    (journal.candidate_pid, journal.candidate_created)
                                {
                                    if crate::elevation::process_creation_time(pid).ok()
                                        == Some(created)
                                        && crate::elevation::validate_process(pid, request.elevated)
                                            .is_ok()
                                    {
                                        let ready = crate::elevation::verify_ready(
                                            pid,
                                            &request.target_hash,
                                            request.elevated,
                                            &request.transaction,
                                        );
                                        *observation.lock().unwrap() = Some(serde_json::json!({
                                            "pid": pid, "created": created,
                                            "elevated": request.elevated,
                                            "real_path_account_session_and_token_verified": true,
                                            "ready_at_observation": ready,
                                        }));
                                        break;
                                    }
                                }
                            }
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                })
            };
            let result = run_worker_with_download(layout, request, |_, hash, staged, progress| {
                require_hash(fixture, hash)?;
                let bytes = std::fs::copy(fixture, staged).map_err(|e| e.to_string())?;
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(staged)
                    .and_then(|file| file.sync_all())
                    .map_err(|e| e.to_string())?;
                require_hash(staged, hash)?;
                progress(bytes, bytes);
                Ok(())
            });
            done.store(true, Ordering::Release);
            observer.join().map_err(|_| "candidate observer stopped")?;
            let journal = layout.load()?;
            let installed_hash = hash_file(&layout.executable)?;
            let ready = ready_evidence(layout, &installed_hash, request.elevated).ok();
            let evidence = serde_json::json!({
                "transaction": request.transaction,
                "operation_succeeded": result.is_ok(),
                "error": result.as_ref().err(),
                "journal": journal,
                "installed_hash": installed_hash,
                "candidate_observation": observation.lock().unwrap().clone(),
                "verified_ready": ready,
            });
            Ok(Attempt {
                result,
                journal,
                evidence,
            })
        }

        fn request_for(
            version: &str,
            hash: &str,
            elevated: bool,
            pid: u32,
            created: u64,
        ) -> Result<Request, String> {
            let request = Request {
                transaction: uuid::Uuid::new_v4().to_string(),
                version: version.into(),
                target_hash: hash.to_ascii_lowercase(),
                elevated,
                parent_pid: pid,
                parent_created: created,
            };
            Request::parse(&request.arguments(START)[1..])
        }

        #[test]
        #[ignore = "changes the live installed application; requires an explicit plan and administrator token"]
        fn installed_update_round_trip() {
            let plan_path = std::env::var_os("AGENTSPADS_LIVE_UPDATE_PLAN")
                .map(PathBuf::from)
                .expect("AGENTSPADS_LIVE_UPDATE_PLAN must identify the approved test plan");
            let plan: Plan = read_json(&plan_path).expect("read the live test plan");
            assert!(
                plan.confirm_live_update,
                "live installation changes require an explicit plan"
            );
            assert!(
                crate::elevation::is_elevated(),
                "run this ignored test with an administrator token"
            );
            crate::managed_tasks::validate_desktop_user().unwrap();
            let layout = layout().expect("validate the actual registered installation");
            assert!(same_path(
                layout.executable.parent().unwrap(),
                &plan.installed_directory
            ));
            require_hash(&layout.executable, &plan.baseline_hash).unwrap();
            require_hash(&plan.baseline_fixture, &plan.baseline_hash).unwrap();
            require_hash(&plan.candidate_fixture, &plan.candidate_hash).unwrap();
            assert!(!plan
                .baseline_hash
                .eq_ignore_ascii_case(&plan.candidate_hash));
            assert!(!same_path(&plan.baseline_fixture, &layout.executable));
            assert!(!same_path(&plan.candidate_fixture, &layout.executable));
            let installation = layout.executable.parent().unwrap().canonicalize().unwrap();
            for fixture in [&plan.baseline_fixture, &plan.candidate_fixture] {
                assert!(
                    !fixture.canonicalize().unwrap().starts_with(&installation),
                    "test fixtures must stay outside the live installation"
                );
            }
            assert_eq!(
                plan.evidence_path.extension().and_then(|v| v.to_str()),
                Some("json")
            );
            assert!(
                !plan.evidence_path.exists(),
                "use a new evidence filename for each case"
            );
            assert!(
                !plan
                    .evidence_path
                    .parent()
                    .unwrap()
                    .canonicalize()
                    .unwrap()
                    .starts_with(&installation),
                "test evidence must stay outside the live installation"
            );
            let parent = open_process(plan.parent_pid, plan.parent_created).unwrap();
            assert!(!wait(&parent, Duration::ZERO));
            crate::elevation::validate_process(plan.parent_pid, plan.elevated).unwrap();
            let baseline = ready_evidence(
                &layout,
                &plan.baseline_hash.to_ascii_lowercase(),
                plan.elevated,
            )
            .unwrap();
            assert_eq!(baseline["pid"].as_u64(), Some(u64::from(plan.parent_pid)));
            assert_eq!(
                baseline["version"].as_str(),
                Some(plan.baseline_version.as_str())
            );
            let request = request_for(
                &plan.candidate_version,
                &plan.candidate_hash,
                plan.elevated,
                plan.parent_pid,
                plan.parent_created,
            )
            .unwrap();

            let first = attempt(&layout, &request, &plan.candidate_fixture);
            let mut restoration = None;
            let mut restore_error = None;
            if let Ok(first) = &first {
                if first.result.is_ok() {
                    let reverse: Result<Attempt, String> = (|| {
                        let journal = first.journal.as_ref().ok_or("missing committed journal")?;
                        if journal.phase != Phase::Committed {
                            return Err("successful update did not commit".into());
                        }
                        let pid = journal.candidate_pid.ok_or("missing replacement process")?;
                        let created = journal
                            .candidate_created
                            .ok_or("missing replacement time")?;
                        let request = request_for(
                            &plan.baseline_version,
                            &plan.baseline_hash,
                            plan.elevated,
                            pid,
                            created,
                        )?;
                        attempt(&layout, &request, &plan.baseline_fixture)
                    })();
                    match reverse {
                        Ok(attempt) => restoration = Some(attempt),
                        Err(error) => restore_error = Some(error),
                    }
                }
            }
            let final_hash = hash_file(&layout.executable);
            let final_ready = ready_evidence(
                &layout,
                &plan.baseline_hash.to_ascii_lowercase(),
                plan.elevated,
            );
            let returned_to_baseline = final_hash
                .as_ref()
                .is_ok_and(|hash| hash.eq_ignore_ascii_case(&plan.baseline_hash))
                && final_ready.as_ref().is_ok_and(|record| {
                    record["version"].as_str() == Some(plan.baseline_version.as_str())
                });
            let expected_outcome = first.as_ref().is_ok_and(|first| {
                let Some(journal) = &first.journal else {
                    return false;
                };
                if plan.expect_success {
                    first.result.is_ok()
                        && journal.phase == Phase::Committed
                        && first.evidence["installed_hash"].as_str()
                            == Some(plan.candidate_hash.to_ascii_lowercase().as_str())
                        && first.evidence["verified_ready"]["version"].as_str()
                            == Some(plan.candidate_version.as_str())
                        && restoration.as_ref().is_some_and(|a| a.result.is_ok())
                } else {
                    first.result.is_err()
                        && journal.phase == Phase::Restored
                        && journal.message.contains("新版未通过启动检查")
                        && first.evidence["candidate_observation"]
                            ["real_path_account_session_and_token_verified"]
                            == true
                        && first.evidence["candidate_observation"]["ready_at_observation"] == false
                }
            });
            let evidence = serde_json::json!({
                "scope": "offline real update transaction; production download transport and UI entry are excluded",
                "installed_directory": plan.installed_directory,
                "elevated": plan.elevated,
                "expected_case": if plan.expect_success { "update_and_return" } else { "created_but_not_ready_rollback" },
                "baseline_before": baseline,
                "first": first.as_ref().ok().map(|a| &a.evidence),
                "first_setup_error": first.as_ref().err(),
                "restoration": restoration.as_ref().map(|a| &a.evidence),
                "restoration_setup_error": restore_error,
                "final_hash": final_hash.as_ref().ok(),
                "final_ready": final_ready.as_ref().ok(),
                "final_ready_error": final_ready.as_ref().err(),
                "returned_to_baseline": returned_to_baseline,
                "passed": returned_to_baseline && expected_outcome,
            });
            write_json(&plan.evidence_path, &evidence).expect("save live transaction evidence");
            assert!(
                returned_to_baseline,
                "baseline recovery requires attention; see {:?}",
                plan.evidence_path
            );
            assert!(
                expected_outcome,
                "unexpected transaction result; see {:?}",
                plan.evidence_path
            );
        }
    }
}

pub fn exit_if_maintenance() {
    #[cfg(windows)]
    {
        let args: Vec<_> = std::env::args().collect();
        let Some(action) = args
            .get(1)
            .filter(|a| matches!(a.as_str(), START | WORKER | RECOVER))
        else {
            return;
        };
        let result = platform::dispatch(action, &args[2..]);
        if let Err(error) = &result {
            crate::logutil::write("managed update failed; see protected update status");
            if action == RECOVER {
                crate::elevation::show_error(error);
            }
            if action == WORKER {
                if let Some(journal) = platform::layout()
                    .ok()
                    .and_then(|layout| layout.load().ok())
                    .flatten()
                {
                    let parent_gone =
                        !platform::process_running(journal.parent_pid, journal.parent_created);
                    if parent_gone && !matches!(journal.phase, Phase::Committed | Phase::Restored) {
                        crate::elevation::show_error(error);
                    }
                }
            }
        }
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }
}

pub fn recover_before_start() -> Result<(), String> {
    #[cfg(windows)]
    {
        platform::recover_before_start()
    }
    #[cfg(not(windows))]
    {
        Ok(())
    }
}

#[cfg(windows)]
pub fn begin(
    info: &crate::updater::UpdateInfo,
    status: &Arc<Mutex<crate::updater::UpdateStatus>>,
) -> Result<(), String> {
    platform::begin(info, status)
}

pub fn last_notice() -> Option<String> {
    #[cfg(windows)]
    {
        if !std::env::current_exe().ok()?.canonicalize().ok()?.eq(
            &crate::elevation::installed_executable()
                .ok()?
                .canonicalize()
                .ok()?,
        ) {
            return None;
        }
        let journal = platform::layout().ok()?.load().ok()??;
        matches!(journal.phase, Phase::Restored | Phase::RecoveryFailed).then_some(journal.message)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        layout: Layout,
        journal: Journal,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("agentpads-managed-update-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let layout = Layout::new(root.join("agentspads.exe")).unwrap();
            layout.ensure().unwrap();
            std::fs::write(&layout.executable, b"old working build").unwrap();
            let transaction = uuid::Uuid::new_v4().to_string();
            std::fs::write(layout.staged(&transaction), b"new candidate build").unwrap();
            let journal = Journal {
                schema: SCHEMA,
                transaction: transaction.clone(),
                version: "1.2.3".into(),
                target_hash: hash_file(&layout.staged(&transaction)).unwrap(),
                previous_hash: hash_file(&layout.executable).unwrap(),
                previous_transaction: None,
                elevated: false,
                parent_pid: 1,
                parent_created: 1,
                worker_pid: 1,
                worker_created: 1,
                owner_sid: "S-1-5-21-42".into(),
                owner_session: 1,
                candidate_pid: None,
                candidate_created: None,
                phase: Phase::Prepared,
                message: String::new(),
            };
            layout.save(&journal).unwrap();
            Self { layout, journal }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.layout.executable.parent().unwrap());
        }
    }

    #[test]
    fn replacing_the_only_official_executable_keeps_its_prior_build() {
        let mut f = Fixture::new();
        f.layout.swap(&mut f.journal).unwrap();
        assert_eq!(
            hash_file(&f.layout.executable).unwrap(),
            f.journal.target_hash
        );
        assert_eq!(
            hash_file(&f.layout.backup(&f.journal.transaction)).unwrap(),
            f.journal.previous_hash
        );
        assert_eq!(f.layout.load().unwrap().unwrap().phase, Phase::NewPlaced);
        f.layout.commit(&mut f.journal).unwrap();
        assert!(f.layout.backup(&f.journal.transaction).exists());
        assert_eq!(f.layout.load().unwrap().unwrap().phase, Phase::Committed);
    }

    #[test]
    fn journal_publication_survives_concurrent_state_readers() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let mut fixture = Fixture::new();
        let done = Arc::new(AtomicBool::new(false));
        let reader = {
            let done = Arc::clone(&done);
            let layout = fixture.layout.clone();
            std::thread::spawn(move || {
                while !done.load(Ordering::Acquire) {
                    let _ = layout.load();
                }
            })
        };
        let mut failure = None;
        for count in 0..100 {
            fixture.journal.message = format!("publication-{count}");
            if let Err(error) = fixture.layout.save(&fixture.journal) {
                failure = Some((count, error));
                break;
            }
        }
        done.store(true, Ordering::Release);
        reader.join().unwrap();
        let quiescent = fixture.layout.save(&fixture.journal);
        assert!(
            failure.is_none(),
            "reader conflict={failure:?}; after reader closes={quiescent:?}"
        );
        quiescent.unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn journal_replace_keeps_existing_read_handles_valid() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        let mut fixture = Fixture::new();
        let original = std::fs::read(fixture.layout.journal()).unwrap();
        let mut reader = OpenOptions::new()
            .read(true)
            .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
            .open(fixture.layout.journal())
            .unwrap();
        fixture.journal.message = "published while another process is reading".into();
        fixture.layout.save(&fixture.journal).unwrap();
        let mut old_bytes = Vec::new();
        reader.read_to_end(&mut old_bytes).unwrap();
        assert_eq!(old_bytes, original);
        assert_eq!(
            fixture.layout.load().unwrap().unwrap().message,
            fixture.journal.message
        );
    }

    #[test]
    fn failed_new_build_restores_exact_old_bytes() {
        let mut f = Fixture::new();
        f.layout.swap(&mut f.journal).unwrap();
        f.layout.restore_files(&mut f.journal).unwrap();
        assert_eq!(
            std::fs::read(&f.layout.executable).unwrap(),
            b"old working build"
        );
        assert!(f
            .layout
            .updates
            .join(format!("failed-{}.exe", f.journal.transaction))
            .exists());
    }

    #[test]
    fn interruption_between_rename_and_journal_write_uses_actual_files() {
        let mut f = Fixture::new();
        std::fs::rename(
            &f.layout.executable,
            f.layout.backup(&f.journal.transaction),
        )
        .unwrap();
        assert_eq!(f.layout.load().unwrap().unwrap().phase, Phase::Prepared);
        f.layout.restore_files(&mut f.journal).unwrap();
        assert_eq!(
            hash_file(&f.layout.executable).unwrap(),
            f.journal.previous_hash
        );
    }

    #[test]
    fn interruption_after_new_rename_before_journal_write_can_restore() {
        let mut f = Fixture::new();
        std::fs::rename(
            &f.layout.executable,
            f.layout.backup(&f.journal.transaction),
        )
        .unwrap();
        std::fs::rename(
            f.layout.staged(&f.journal.transaction),
            &f.layout.executable,
        )
        .unwrap();
        f.layout.restore_files(&mut f.journal).unwrap();
        assert_eq!(
            hash_file(&f.layout.executable).unwrap(),
            f.journal.previous_hash
        );
    }

    #[test]
    fn changed_target_and_bad_backup_are_not_destroyed() {
        let mut f = Fixture::new();
        f.layout.swap(&mut f.journal).unwrap();
        std::fs::write(&f.layout.executable, b"another installation").unwrap();
        assert!(f.layout.restore_files(&mut f.journal).is_err());
        assert_eq!(
            std::fs::read(&f.layout.executable).unwrap(),
            b"another installation"
        );
        assert!(f.layout.backup(&f.journal.transaction).exists());
        std::fs::write(f.layout.backup(&f.journal.transaction), b"damaged backup").unwrap();
        assert!(f.layout.restore_files(&mut f.journal).is_err());
        assert!(f.layout.backup(&f.journal.transaction).exists());
    }

    #[test]
    fn failed_download_cannot_move_the_current_executable() {
        let mut f = Fixture::new();
        std::fs::write(
            f.layout.staged(&f.journal.transaction),
            b"incomplete download",
        )
        .unwrap();
        assert!(f.layout.swap(&mut f.journal).is_err());
        assert_eq!(
            hash_file(&f.layout.executable).unwrap(),
            f.journal.previous_hash
        );
        assert!(!f.layout.backup(&f.journal.transaction).exists());
    }

    #[test]
    fn commit_keeps_current_backup_and_only_cleans_previous_committed_backup() {
        let mut f = Fixture::new();
        let prior = uuid::Uuid::new_v4().to_string();
        std::fs::write(f.layout.backup(&prior), b"even older build").unwrap();
        f.journal.previous_transaction = Some(prior.clone());
        f.layout.swap(&mut f.journal).unwrap();
        assert!(f.layout.backup(&prior).exists());
        f.layout.commit(&mut f.journal).unwrap();
        assert!(!f.layout.backup(&prior).exists());
        assert!(f.layout.backup(&f.journal.transaction).exists());
    }

    #[test]
    fn rejects_path_injection_and_incomplete_transaction_identity() {
        let f = Fixture::new();
        let base = vec![
            f.journal.transaction.clone(),
            "1.2.3".into(),
            f.journal.target_hash.clone(),
            "normal".into(),
            "5".into(),
            "7".into(),
        ];
        assert!(Request::parse(&base).is_ok());
        for (index, value) in [
            (0, "../../outside"),
            (1, "1.2.3/evil"),
            (2, "not-hash"),
            (3, "system"),
            (4, "0"),
            (5, "0"),
        ] {
            let mut invalid = base.clone();
            invalid[index] = value.into();
            assert!(Request::parse(&invalid).is_err());
        }
        let mut invalid = f.journal.clone();
        invalid.previous_transaction = Some("../outside".into());
        assert!(validate_journal(&invalid).is_err());
    }

    #[test]
    fn damaged_journal_is_reported_without_cleaning_any_program() {
        let f = Fixture::new();
        std::fs::write(f.layout.journal(), b"{broken").unwrap();
        assert!(f.layout.load().is_err());
        assert!(f.layout.executable.exists());
        assert!(f.layout.staged(&f.journal.transaction).exists());
    }

    #[cfg(windows)]
    #[test]
    fn antivirus_lock_during_placement_keeps_old_build_recoverable() {
        use std::os::windows::fs::OpenOptionsExt;
        let mut f = Fixture::new();
        let _locked = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(f.layout.staged(&f.journal.transaction))
            .unwrap();
        assert!(f.layout.swap(&mut f.journal).is_err());
        assert_eq!(
            hash_file(&f.layout.executable).unwrap(),
            f.journal.previous_hash
        );
        f.layout.restore_files(&mut f.journal).unwrap();
        assert_eq!(
            hash_file(&f.layout.executable).unwrap(),
            f.journal.previous_hash
        );
    }

    #[cfg(windows)]
    #[test]
    fn journal_write_failure_cannot_start_replacing_old_file() {
        use std::os::windows::fs::OpenOptionsExt;
        let mut f = Fixture::new();
        let _locked = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(f.layout.journal())
            .unwrap();
        assert!(f.layout.swap(&mut f.journal).is_err());
        assert_eq!(
            hash_file(&f.layout.executable).unwrap(),
            f.journal.previous_hash
        );
        assert!(!f.layout.backup(&f.journal.transaction).exists());
        f.layout.restore_files(&mut f.journal).unwrap();
        assert_eq!(
            hash_file(&f.layout.executable).unwrap(),
            f.journal.previous_hash
        );
    }

    #[cfg(windows)]
    #[test]
    fn read_only_probe_detects_an_active_transaction() {
        let f = Fixture::new();
        assert!(f.layout.idle());
        let lock = f.layout.lock().unwrap();
        assert!(!f.layout.idle());
        assert!(f.layout.lock().is_err());
        drop(lock);
        assert!(f.layout.idle());
    }

    #[test]
    fn recovery_is_idempotent_and_rejects_partial_process_identity() {
        let mut f = Fixture::new();
        f.layout.swap(&mut f.journal).unwrap();
        f.layout.restore_files(&mut f.journal).unwrap();
        f.layout.restore_files(&mut f.journal).unwrap();
        assert_eq!(
            hash_file(&f.layout.executable).unwrap(),
            f.journal.previous_hash
        );
        f.journal.candidate_pid = Some(42);
        assert!(validate_journal(&f.journal).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn replacement_preserves_a_running_image_for_recovery_startup() {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut f = Fixture::new();
        let system_root = std::env::var_os("SystemRoot").expect("Windows system directory");
        std::fs::copy(
            PathBuf::from(system_root).join("System32/cmd.exe"),
            &f.layout.executable,
        )
        .unwrap();
        f.journal.previous_hash = hash_file(&f.layout.executable).unwrap();
        let mut child = ChildGuard(
            Command::new(&f.layout.executable)
                .args(["/d", "/q", "/k"])
                .current_dir(f.layout.executable.parent().unwrap())
                .creation_flags(0x0800_0000)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        std::thread::sleep(Duration::from_millis(200));
        assert!(child.0.try_wait().unwrap().is_none());
        f.layout.swap(&mut f.journal).unwrap();
        assert_eq!(
            hash_file(&f.layout.executable).unwrap(),
            f.journal.target_hash
        );
        assert_eq!(
            hash_file(&f.layout.backup(&f.journal.transaction)).unwrap(),
            f.journal.previous_hash
        );
        assert!(child.0.try_wait().unwrap().is_none());
    }
}
