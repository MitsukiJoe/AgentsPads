//! Windows only: UIPI blocks `SendInput` into windows of higher integrity
//! (Task Manager, elevated apps), so users can opt into running elevated.
//! One UAC prompt copies this exe to `%ProgramFiles%\AgentsPads\agentspads.exe`
//! (ACL inherited from Program Files) and registers an on-demand "highest
//! privileges" task that runs that copy. Later launches, including login
//! autostart, re-exec through the task with no prompt. Task XML is written
//! only in that directory and removed after `schtasks /Create`.
//!
//! The elevated process must not create or modify anything under the user
//! profile, Startup folder, or `AGENTPAD_DATA_DIR`. Admin mode is the marker
//! `%ProgramFiles%\AgentsPads\run_as_admin.txt`, written by the elevated
//! installer and removed with that directory. Theme, the guide flag, and logs
//! live in `state\` beside it and keep the inherited Program Files ACL, so
//! ordinary users can still open logs. Login autostart is applied by the
//! unelevated starter before re-exec, using that process's own executable.

#[cfg(any(windows, test))]
const TASK_NAME: &str = "AgentsPads Elevated";
#[cfg(windows)]
const MARKER_FILE: &str = "run_as_admin.txt";
/// Login autostart in admin mode is a second task with a logon trigger; this
/// file mirrors whether it is enabled so the settings page can show it.
#[cfg(any(windows, test))]
const AUTOSTART_TASK_NAME: &str = "AgentsPads Elevated Autostart";
#[cfg(windows)]
const AUTOSTART_MARKER_FILE: &str = "autostart.txt";
#[cfg(any(windows, test))]
const AUTOSTART_FLAG: &str = "--admin-autostart";
/// Login is busy; this app can wait behind everything else.
#[cfg(any(windows, test))]
const LOGON_DELAY: &str = "PT30S";
#[cfg(windows)]
const STATE_DIR: &str = "state";
#[cfg(windows)]
const TASK_XML_NAME: &str = "agentspads-task.xml";
#[cfg(any(windows, test))]
const SECRET_DIR: &str = "secret";
#[cfg(windows)]
const SOURCE_EXE_FILE: &str = "ordinary-exe-path.bin";
#[cfg(any(windows, test))]
const STAGE_DISABLE_FLAG: &str = "--stage-admin-disable";
#[cfg(any(windows, test))]
const DISABLE_WORKER_FLAG: &str = "--admin-disable-worker";
#[cfg(windows)]
const DISABLE_WORKER_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
#[cfg(windows)]
const DISABLE_STAGE_WAIT: std::time::Duration = std::time::Duration::from_secs(15);
#[cfg(windows)]
const DISABLE_CLEANUP_READY_WAIT: std::time::Duration = std::time::Duration::from_secs(12);
#[cfg(any(windows, test))]
const DISABLE_CLEANUP_READY_FILE: &str = "cleanup-ready.txt";
#[cfg(any(windows, test))]
const DISABLE_CLEANUP_REGISTRATION_FILE: &str = "cleanup-registration.txt";
const RELAUNCH_FLAG: &str = "--elevated";
/// Not `--after-update`: that flag means a finished update (delete `*.old`)
/// and never waits for the parent process.
#[cfg(any(windows, test))]
const HANDOFF_FLAG: &str = "--handoff";
#[cfg(windows)]
const HANDOFF_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
#[cfg(any(windows, test))]
const STARTUP_ERROR_FILE: &str = "startup_error.txt";
#[cfg(any(windows, test))]
const STARTUP_ERROR_MAX: usize = 1024;
/// Ordinary readers ignore anything older than a few minutes.
#[cfg(any(windows, test))]
const STARTUP_ERROR_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(180);
#[cfg(windows)]
static HANDOFF_FAILURE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
#[cfg(any(windows, test))]
const INSTALL_FLAG: &str = "--install-admin-task";
#[cfg(any(windows, test))]
const REMOVE_FLAG: &str = "--remove-admin-task";

/// The elevated child may start before the parent has released the port.
pub fn relaunched() -> bool {
    std::env::args().any(|a| a == RELAUNCH_FLAG)
}

/// `--handoff <pid>` from the settings switch: block until that process exits
/// (or the timeout elapses) so it can release port 9618 before we relaunch.
pub fn wait_for_handoff_parent() -> Result<(), &'static str> {
    #[cfg(windows)]
    {
        if let Some(pid) = handoff_parent_pid(std::env::args()) {
            return wait_for_pid(pid, HANDOFF_WAIT);
        }
    }
    Ok(())
}

pub fn invalid_elevation_attempt() -> bool {
    cfg!(windows) && reject_ordinary_attempt(relaunched(), is_elevated())
}

fn reject_ordinary_attempt(attempt: bool, elevated: bool) -> bool {
    attempt && !elevated
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelaunchOutcome {
    ContinueOrdinaryStartup,
    HandoffStarted,
    ProtectedIdentityStoreFailed,
}

/// Call before binding the port.
pub fn relaunch_if_needed() -> RelaunchOutcome {
    #[cfg(windows)]
    {
        if !enabled() || is_elevated() {
            return RelaunchOutcome::ContinueOrdinaryStartup;
        }
        let attempt = std::time::SystemTime::now();
        let task_started = run_task();
        if task_started && wait_for_ready(None) {
            return RelaunchOutcome::HandoffStarted;
        }
        let launched = launch_protected_elevated();
        if let Ok(process) = &launched {
            if wait_for_ready(Some(process)) {
                return RelaunchOutcome::HandoffStarted;
            }
        }
        let detail = recent_startup_error(attempt);
        let outcome = failed_handoff_outcome(detail.as_deref());
        let reason = explain_handoff_failure(
            task_started,
            matches!(launched, Err(LaunchError::Cancelled)),
            detail.as_deref(),
        );
        crate::logutil::write("admin relaunch failed");
        note_handoff_failure(&reason);
        outcome
    }
    #[cfg(not(windows))]
    {
        RelaunchOutcome::ContinueOrdinaryStartup
    }
}

/// Failure text for the settings row. Empty on macOS and after a clean handoff.
#[cfg(windows)]
pub fn handoff_failure() -> Option<String> {
    HANDOFF_FAILURE.lock().unwrap().clone()
}

#[cfg(windows)]
pub fn note_handoff_failure(reason: &str) {
    *HANDOFF_FAILURE.lock().unwrap() = Some(reason.to_string());
}

/// Spawn the current exe as an ordinary process, then the caller should exit.
/// The child waits for this pid and runs `relaunch_if_needed`.
#[cfg(windows)]
pub fn spawn_unelevated_handoff() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    std::process::Command::new(exe)
        .arg(HANDOFF_FLAG)
        .arg(std::process::id().to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// Elevated process only. One short line under Program Files `state\`.
/// Ordinary and non-Windows calls do not touch the user profile.
pub fn note_startup_failure(kind: &str, detail: &str) {
    #[cfg(windows)]
    {
        if !is_elevated() {
            return;
        }
        let Some(dir) = protected_state_dir() else {
            return;
        };
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let text = startup_failure_text(kind, detail);
        let _ = std::fs::write(
            dir.join(STARTUP_ERROR_FILE),
            startup_error_line(&text, secs),
        );
    }
    #[cfg(not(windows))]
    {
        let _ = (kind, detail);
    }
}

/// `--install-admin-task` / `--remove-admin-task`: do the work and exit.
pub fn exit_if_admin_maintenance() {
    #[cfg(windows)]
    {
        let action = early_action(std::env::args());
        let code = match action {
            EarlyAction::None => return,
            EarlyAction::Install(user, autostart) => {
                if install_protected(&user, autostart) {
                    0
                } else {
                    1
                }
            }
            EarlyAction::Remove => {
                if remove_protected().is_ok() {
                    0
                } else {
                    1
                }
            }
            EarlyAction::StageDisable(pid, created) => match stage_disable_worker(pid, created) {
                Ok(()) => 0,
                Err(reason) => stage_disable_helper_exit_code(&reason),
            },
            EarlyAction::DisableWorker(pid, created) => match run_disable_worker(pid, created) {
                Ok(()) => 0,
                Err(DisableWorkerFailure::NeedsDialog(reason)) => {
                    show_disable_worker_failure(&reason);
                    1
                }
                Err(DisableWorkerFailure::CleanupBlocked) => {
                    STAGE_HELPER_CLEANUP_BLOCKED_EXIT as i32
                }
                Err(DisableWorkerFailure::ReportedToStage) => 1,
            },
            EarlyAction::Bad => 1,
        };
        std::process::exit(code);
    }
}

/// Marker file plus the protected copy. Both live under Program Files, so an
/// ordinary process can read them but cannot turn admin mode on by writing a
/// user-profile file. A missing scheduled task is noticed when the relaunch
/// itself fails; this check does not spawn `schtasks`.
#[cfg(windows)]
pub fn enabled() -> bool {
    let Some((dir, exe)) = protected_target() else {
        return false;
    };
    admin_mode_enabled(marker_is_set(&dir), exe.is_file())
}

pub fn is_elevated() -> bool {
    // Tests always run as "not elevated": CI Windows runners are administrators, and
    // an elevated test process would take the Program Files code paths.
    #[cfg(all(windows, not(test)))]
    {
        unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() }
    }
    #[cfg(any(not(windows), test))]
    {
        false
    }
}

/// One UAC prompt, unless this process is already elevated.
#[cfg(windows)]
pub fn install_task(autostart: bool) -> Result<(), &'static str> {
    let (Ok(exe), Some(user)) = (std::env::current_exe(), windows_user()) else {
        return Err("无法确定当前程序或用户");
    };
    let mut args = vec![INSTALL_FLAG, user.as_str()];
    if autostart {
        args.push(AUTOSTART_FLAG);
    }
    shell_exec_wait(exe.as_os_str(), &join_windows_args(&args))
}

/// Begin disabling admin mode without deleting the image of a running process.
/// The worker is staged outside Program Files, waits for this UI process to exit,
/// then removes the tasks and protected tree before resuming an ordinary child.
#[cfg(windows)]
pub fn begin_disable_admin() -> Result<(), String> {
    if !enabled() {
        return Err("管理员模式状态不可用".to_string());
    }
    let (dir, protected_exe) =
        protected_target().ok_or_else(|| "无法验证管理员安装目录".to_string())?;
    ordinary_source_exe(&dir, &protected_exe)?;
    let parent_pid = std::process::id();
    let parent_created = current_process_creation_time()?;
    if is_elevated() {
        return stage_disable_worker(parent_pid, parent_created);
    }
    let params = join_windows_args(&[
        STAGE_DISABLE_FLAG,
        &parent_pid.to_string(),
        &parent_created.to_string(),
    ]);
    let helper = shell_exec(protected_exe.as_os_str(), &params, true).map_err(|err| match err {
        LaunchError::Cancelled => "已取消 UAC".to_string(),
        LaunchError::Failed => "无法启动管理员关闭程序".to_string(),
    })?;
    wait_for_process_exit(&helper, std::time::Duration::from_secs(30))?;
    let mut code = 1;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::GetExitCodeProcess;
    unsafe {
        GetExitCodeProcess(HANDLE(helper.as_raw_handle()), &mut code)
            .map_err(|_| "无法确认管理员关闭程序结果".to_string())?;
    }
    if code == 0 {
        Ok(())
    } else {
        Err(stage_disable_helper_failure_message(code))
    }
}

/// Identity file of the elevated instance. Its pairing secret must not be
/// readable by ordinary same-user processes, which could otherwise authenticate
/// and inject input into elevated windows through this instance.
///
/// Lives under `%ProgramFiles%\AgentsPads\secret`: ordinary users can neither
/// create nor replace entries in that tree, so nobody can pre-create it or turn
/// it into a link. The directory and file are then re-owned by Administrators
/// (the creator may be the user SID, which a non-elevated token of the same user
/// would match) and the DACL is cut to Administrators/SYSTEM. The empty file is
/// locked before the secret is written. Any failure is an error; never fall
/// back to the ordinary instance's key. Disabling admin mode deletes the tree.
#[cfg(windows)]
pub fn admin_identity_path() -> std::io::Result<std::path::PathBuf> {
    let err = |msg: &str| std::io::Error::other(msg.to_string());
    if !is_elevated() {
        return Err(err("not elevated"));
    }
    let (dir, _) = protected_target().ok_or_else(|| err("no program files"))?;
    prepare_admin_identity_at(&dir.join(SECRET_DIR))
}

#[cfg(windows)]
fn prepare_admin_identity_at(dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let err = |msg: &str| std::io::Error::other(msg.to_string());
    if !no_reparse_ancestors(dir) {
        return Err(err("invalid admin identity path"));
    }
    std::fs::create_dir_all(dir)?;
    if !no_reparse_ancestors(dir) {
        return Err(err("invalid admin identity path"));
    }
    let file = dir.join("identity.json");
    let mut file_exists = false;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name() == std::ffi::OsStr::new("identity.json") {
            let file_type = entry.file_type()?;
            if !file_type.is_file() || file_type.is_symlink() {
                return Err(err("invalid admin identity file"));
            }
            file_exists = true;
            break;
        }
    }

    set_administrators_owner(dir)?;
    run_icacls(
        dir,
        &[
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-32-544:(OI)(CI)F",
            "*S-1-5-18:(OI)(CI)F",
        ],
    )?;

    if !file_exists {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)
        {
            Ok(created) => drop(created),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }

    run_icacls(
        &file,
        &[
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-32-544:F",
            "*S-1-5-18:F",
        ],
    )?;
    set_administrators_owner(&file)?;
    Ok(file)
}

#[cfg(windows)]
fn set_administrators_owner(path: &std::path::Path) -> std::io::Result<()> {
    run_icacls(path, &["/setowner", "*S-1-5-32-544"])
}

#[cfg(windows)]
fn run_icacls(path: &std::path::Path, args: &[&str]) -> std::io::Result<()> {
    let path = path
        .to_str()
        .ok_or_else(|| std::io::Error::other("bad path"))?;
    let mut command_args = Vec::with_capacity(args.len() + 1);
    command_args.push(path);
    command_args.extend_from_slice(args);
    if system32("icacls.exe", &command_args) {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "icacls failed ({})",
            args.join(" ")
        )))
    }
}
/// Running an existing task needs no elevation and shows no prompt.
#[cfg(windows)]
pub fn run_task() -> bool {
    schtasks(&["/Run", "/TN", TASK_NAME])
}

#[cfg(windows)]
fn flag_is_true(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|value| value.trim() == "true")
}

#[cfg(windows)]
fn marker_is_set(dir: &std::path::Path) -> bool {
    flag_is_true(&dir.join(MARKER_FILE))
}

#[cfg(windows)]
fn write_flag(dir: &std::path::Path, file: &str, on: bool) -> bool {
    std::fs::write(dir.join(file), on.to_string()).is_ok()
}

/// Admin-mode login autostart state, from the protected directory.
#[cfg(windows)]
pub fn autostart_enabled() -> bool {
    protected_target().is_some_and(|(dir, _)| flag_is_true(&dir.join(AUTOSTART_MARKER_FILE)))
}

/// Enable or disable the logon-trigger task. Only the elevated instance may do
/// this (no UAC prompt, and ordinary processes cannot change the task); nothing
/// in the user profile is touched.
#[cfg(windows)]
pub fn set_autostart(on: bool) -> bool {
    let Some((dir, _)) = protected_target() else {
        return false;
    };
    is_elevated()
        && schtasks(&[
            "/Change",
            "/TN",
            AUTOSTART_TASK_NAME,
            if on { "/ENABLE" } else { "/DISABLE" },
        ])
        && write_flag(&dir, AUTOSTART_MARKER_FILE, on)
}

/// Runtime files for the elevated process (theme, guide flag, logs). Not locked
/// down like `secret\`: callers create it and inherit the Program Files ACL.
#[cfg(windows)]
pub(crate) fn protected_state_dir() -> Option<std::path::PathBuf> {
    let path = protected_target()?.0.join(STATE_DIR);
    no_reparse_ancestors(&path).then_some(path)
}

#[cfg(windows)]
fn install_protected(user: &str, autostart: bool) -> bool {
    if !is_elevated() {
        return false;
    }
    let (Ok(src), Some((dir, dest))) = (std::env::current_exe(), protected_target()) else {
        return false;
    };
    if std::fs::create_dir_all(&dir).is_err() || install_copy(&src, &dest).is_err() {
        return false;
    }
    if store_ordinary_source(&dir, &src, &dest).is_err() {
        return false;
    }
    let Some(command) = dest.to_str() else {
        return false;
    };
    let xml_path = dir.join(TASK_XML_NAME);
    let create = |name: &str, xml: String| {
        write_utf16(&xml_path, &xml).is_ok()
            && xml_path
                .to_str()
                .is_some_and(|path| schtasks(&["/Create", "/TN", name, "/XML", path, "/F"]))
    };
    let ok = create(TASK_NAME, task_xml(command, Some(user), None))
        && create(
            AUTOSTART_TASK_NAME,
            task_xml(command, Some(user), Some(autostart)),
        );
    let _ = std::fs::remove_file(xml_path);
    // Markers only after the tasks exist, so a failed install cannot arm admin mode
    // against a leftover task. Not secrets: they inherit the Program Files ACL.
    ok && write_flag(&dir, AUTOSTART_MARKER_FILE, autostart) && write_flag(&dir, MARKER_FILE, true)
}

#[cfg(windows)]
fn remove_protected() -> Result<(), String> {
    if !is_elevated() {
        return Err("需要管理员权限才能移除管理员安装".to_string());
    }
    delete_admin_tasks()?;
    let (dir, _) = protected_target().ok_or_else(|| "无法验证管理员安装目录".to_string())?;
    if dir.file_name().and_then(|name| name.to_str()) != Some("AgentsPads") {
        return Err("管理员安装目录校验失败".to_string());
    }
    std::fs::remove_dir_all(&dir).map_err(|_| "无法删除受保护的管理员目录".to_string())?;
    if dir.exists() {
        return Err("受保护的管理员目录仍然存在".to_string());
    }
    Ok(())
}

#[cfg(windows)]
fn store_ordinary_source(
    dir: &std::path::Path,
    source: &std::path::Path,
    protected_exe: &std::path::Path,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let failure = || std::io::Error::other("invalid ordinary source executable");
    if same_file(source, protected_exe)
        || !source.is_absolute()
        || !source.is_file()
        || !no_reparse_ancestors(source)
    {
        return Err(failure());
    }
    let metadata = dir.join(SOURCE_EXE_FILE);
    match std::fs::symlink_metadata(&metadata) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {}
        Ok(_) => return Err(failure()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let bytes: Vec<u8> = source
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect();
    std::fs::write(&metadata, bytes)?;
    set_administrators_owner(&metadata)
}

#[cfg(windows)]
fn ordinary_source_exe(
    dir: &std::path::Path,
    protected_exe: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    use std::os::windows::ffi::OsStringExt;
    let invalid = || "管理员安装缺少有效的普通程序路径".to_string();
    let metadata = dir.join(SOURCE_EXE_FILE);
    let meta = std::fs::symlink_metadata(&metadata).map_err(|_| invalid())?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.len() == 0
        || meta.len() > 2048
        || !no_reparse_ancestors(&metadata)
    {
        return Err(invalid());
    }
    let bytes = std::fs::read(&metadata).map_err(|_| invalid())?;
    if bytes.len() % 2 != 0 {
        return Err(invalid());
    }
    let wide: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    if wide.contains(&0) {
        return Err(invalid());
    }
    let source = std::path::PathBuf::from(std::ffi::OsString::from_wide(&wide));
    if !source.is_absolute()
        || !source.is_file()
        || !no_reparse_ancestors(&source)
        || same_file(&source, protected_exe)
        || source.starts_with(dir)
    {
        return Err(invalid());
    }
    Ok(source)
}

/// Delete first so `File::create` inherits the directory ACL. Truncating an
/// existing file would keep the previous DACL.
#[cfg(windows)]
static DISABLE_STAGE_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(windows)]
static PENDING_DISABLE_WORKER: std::sync::Mutex<Option<std::process::Child>> =
    std::sync::Mutex::new(None);

#[cfg(windows)]
struct DisableStageLatch {
    keep_active: bool,
}

#[cfg(windows)]
impl Drop for DisableStageLatch {
    fn drop(&mut self) {
        if !self.keep_active {
            DISABLE_STAGE_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
        }
    }
}

#[cfg(windows)]
fn ensure_no_pending_disable_worker() -> Result<(), String> {
    let mut pending = PENDING_DISABLE_WORKER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(worker) = pending.as_mut() else {
        return Ok(());
    };
    match worker.try_wait() {
        Ok(Some(_)) => {
            *pending = None;
            Ok(())
        }
        Ok(None) => {
            Err("上一次关闭流程仍在结束；管理员模式保持开启，请稍后再手动尝试。".to_string())
        }
        Err(_) => Err("无法确认上一次关闭流程是否已结束；管理员模式保持开启。".to_string()),
    }
}

#[cfg(windows)]
fn retain_pending_disable_worker(worker: std::process::Child) {
    let mut pending = PENDING_DISABLE_WORKER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if pending.is_none() {
        *pending = Some(worker);
    }
}

#[cfg(windows)]
fn stage_disable_worker(parent_pid: u32, parent_created: u64) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    if !is_elevated() || parent_pid == 0 || parent_created == 0 {
        return Err("需要管理员权限才能关闭管理员模式".to_string());
    }
    if DISABLE_STAGE_ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("已有一次管理员模式关闭正在进行".to_string());
    }
    let mut latch = DisableStageLatch { keep_active: false };
    ensure_no_pending_disable_worker()?;
    let (root, protected_exe) =
        protected_target().ok_or_else(|| "无法验证管理员安装目录".to_string())?;
    ordinary_source_exe(&root, &protected_exe)?;
    if !protected_exe.is_file() || !no_reparse_ancestors(&protected_exe) {
        return Err("管理员程序副本不可用".to_string());
    }
    let program_data =
        program_data_directory().ok_or_else(|| "无法确定安全的临时工作目录".to_string())?;
    if !no_reparse_ancestors(&program_data) {
        return Err("临时工作目录路径无效".to_string());
    }
    let worker_dir = program_data.join(format!("AgentsPads-disable-{}", uuid::Uuid::new_v4()));
    create_private_worker_directory(&worker_dir)?;
    let worker_exe = worker_dir.join("agentspads-disable-worker.exe");
    let mut worker = None;
    let mut worker_identity = None;
    let mut stage_result = (|| {
        if !no_reparse_ancestors(&worker_dir) {
            return Err("临时目录路径无效".to_string());
        }
        validate_admin_system_acl(&worker_dir, true)?;
        install_copy(&protected_exe, &worker_exe)
            .map_err(|_| "无法准备临时关闭程序".to_string())?;
        set_administrators_owner(&worker_exe).map_err(|_| "无法设置临时程序所有者".to_string())?;
        if !no_reparse_ancestors(&worker_exe) {
            return Err("临时关闭程序路径无效".to_string());
        }
        validate_disable_worker_paths(&worker_dir, &worker_exe)?;
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut child = std::process::Command::new(&worker_exe)
            .arg(DISABLE_WORKER_FLAG)
            .arg(parent_pid.to_string())
            .arg(parent_created.to_string())
            .current_dir(&program_data)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|_| "无法启动临时关闭程序".to_string())?;
        let worker_pid = child.id();
        let worker_created = match process_creation_time(worker_pid) {
            Ok(created) => created,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                worker = Some(child);
                return Err("无法验证临时关闭程序身份".to_string());
            }
        };
        worker_identity = Some((worker_pid, worker_created));
        let result =
            wait_for_disable_worker_stage(&mut child, &worker_dir, parent_pid, parent_created);
        worker = Some(child);
        result
    })();
    if stage_result.is_err() {
        if let Some(mut child) = worker {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut exited = false;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        exited = true;
                        break;
                    }
                    Ok(None) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    _ => break,
                }
            }
            if exited {
                if let Some(worker_identity) = worker_identity {
                    if let Err(cleanup_reason) = cleanup_failed_staged_worker(
                        &worker_dir,
                        &worker_exe,
                        Some(worker_identity),
                    ) {
                        if let Err(reason) = &mut stage_result {
                            reason.push_str("；");
                            reason.push_str(&cleanup_reason);
                        }
                    }
                } else if let Err(reason) = &mut stage_result {
                    reason.push_str("；无法验证本次临时关闭程序身份，临时目录保留");
                }
            } else {
                retain_pending_disable_worker(child);
            }
        } else {
            if let Err(cleanup_reason) =
                cleanup_failed_staged_worker(&worker_dir, &worker_exe, None)
            {
                if let Err(reason) = &mut stage_result {
                    reason.push_str("；");
                    reason.push_str(&cleanup_reason);
                }
            }
        }
    }
    if stage_result.is_ok() {
        latch.keep_active = true;
    }
    stage_result
}

#[cfg(windows)]
fn create_private_worker_directory(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    #[repr(C)]
    struct SecurityAttributes {
        length: u32,
        descriptor: *mut std::ffi::c_void,
        inherit_handle: i32,
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            string: *const u16,
            revision: u32,
            descriptor: *mut *mut std::ffi::c_void,
            size: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateDirectoryW(path: *const u16, attributes: *mut SecurityAttributes) -> i32;
        fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    }
    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let sddl: Vec<u16> = "O:BAG:BAD:P(A;OICI;FA;;;BA)(A;OICI;FA;;;SY)"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = std::ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if converted == 0 || descriptor.is_null() {
        return Err("无法创建受保护的临时目录安全描述符".to_string());
    }
    let mut attributes = SecurityAttributes {
        length: std::mem::size_of::<SecurityAttributes>() as u32,
        descriptor,
        inherit_handle: 0,
    };
    let created = unsafe { CreateDirectoryW(path_wide.as_ptr(), &mut attributes) };
    unsafe {
        let _ = LocalFree(descriptor);
    }
    if created == 0 {
        return Err("无法原子创建受保护的临时目录".to_string());
    }
    Ok(())
}

#[cfg(windows)]
fn validate_admin_system_acl(path: &std::path::Path, directory: bool) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    #[repr(C)]
    struct AclHeader {
        revision: u8,
        sbz1: u8,
        size: u16,
        ace_count: u16,
        sbz2: u16,
    }
    #[repr(C)]
    struct AceHeader {
        ace_type: u8,
        flags: u8,
        size: u16,
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn GetNamedSecurityInfoW(
            name: *mut u16,
            object_type: u32,
            security_information: u32,
            owner: *mut *mut std::ffi::c_void,
            group: *mut *mut std::ffi::c_void,
            dacl: *mut *mut AclHeader,
            sacl: *mut *mut std::ffi::c_void,
            descriptor: *mut *mut std::ffi::c_void,
        ) -> u32;
        fn ConvertSidToStringSidW(sid: *mut std::ffi::c_void, string_sid: *mut *mut u16) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    }
    unsafe fn sid_text(sid: *mut std::ffi::c_void) -> Option<String> {
        let mut text = std::ptr::null_mut();
        if ConvertSidToStringSidW(sid, &mut text) == 0 || text.is_null() {
            return None;
        }
        let mut length = 0usize;
        while *text.add(length) != 0 {
            length += 1;
        }
        let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        let _ = LocalFree(text.cast());
        Some(value)
    }
    let mut name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut owner = std::ptr::null_mut();
    let mut dacl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            name.as_mut_ptr(),
            1,
            0x1 | 0x4,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 || descriptor.is_null() {
        return Err("无法读取临时目录安全属性".to_string());
    }
    let result = (|| {
        if unsafe { sid_text(owner) }.as_deref() != Some("S-1-5-32-544") || dacl.is_null() {
            return Err("临时目录所有者或访问控制列表不安全".to_string());
        }
        let acl = unsafe { &*dacl };
        if acl.ace_count != 2 {
            return Err("临时目录访问控制列表包含额外主体".to_string());
        }
        let mut seen_admin = false;
        let mut seen_system = false;
        let mut ace = unsafe { dacl.cast::<u8>().add(std::mem::size_of::<AclHeader>()) };
        for _ in 0..acl.ace_count {
            let header = unsafe { &*ace.cast::<AceHeader>() };
            if header.ace_type != 0 || header.size < 8 {
                return Err("临时目录访问控制列表包含非允许项".to_string());
            }
            let mask = unsafe { std::ptr::read_unaligned(ace.add(4).cast::<u32>()) };
            let sid = unsafe { ace.add(8).cast::<std::ffi::c_void>() };
            let principal =
                unsafe { sid_text(sid) }.ok_or_else(|| "无法验证临时目录访问主体".to_string())?;
            let expected_flags = if directory { 0x03 } else { 0x10 };
            if mask != 0x001F_01FF || header.flags != expected_flags {
                return Err("临时目录访问权限不符合预期".to_string());
            }
            match principal.as_str() {
                "S-1-5-32-544" if !seen_admin => seen_admin = true,
                "S-1-5-18" if !seen_system => seen_system = true,
                _ => return Err("临时目录访问控制列表包含额外主体".to_string()),
            }
            ace = unsafe { ace.add(header.size as usize) };
        }
        if !seen_admin || !seen_system {
            return Err("临时目录缺少管理员或 SYSTEM 权限".to_string());
        }
        Ok(())
    })();
    unsafe {
        let _ = LocalFree(descriptor);
    }
    result
}

#[cfg(windows)]
fn validate_disable_worker_paths(
    worker_dir: &std::path::Path,
    worker_exe: &std::path::Path,
) -> Result<(), String> {
    let invalid = || "临时关闭程序路径或安全属性无效".to_string();
    let program_data = program_data_directory().ok_or_else(invalid)?;
    if !no_reparse_ancestors(&program_data)
        || !no_reparse_ancestors(worker_dir)
        || !no_reparse_ancestors(worker_exe)
    {
        return Err(invalid());
    }
    if !valid_disable_worker_boundary(&program_data, worker_dir, worker_exe) {
        return Err(invalid());
    }
    validate_admin_system_acl(worker_dir, true)?;
    validate_admin_system_acl(worker_exe, false)?;
    Ok(())
}

#[cfg(windows)]
fn read_disable_cleanup_registration(
    worker_dir: &std::path::Path,
) -> Result<Option<DisableCleanupRegistration>, String> {
    let path = worker_dir.join(DISABLE_CLEANUP_REGISTRATION_FILE);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("无法读取临时目录清理身份记录".to_string()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || !no_reparse_ancestors(&path) {
        return Err("临时目录清理身份记录路径无效".to_string());
    }
    validate_admin_system_acl(&path, false)?;
    if metadata.len() > 256 {
        return Err("临时目录清理身份记录过长".to_string());
    }
    let text =
        std::fs::read_to_string(path).map_err(|_| "无法读取临时目录清理身份记录".to_string())?;
    parse_disable_cleanup_registration(&text, "PENDING").map(Some)
}

#[cfg(windows)]
fn validate_disable_worker_directory(worker_dir: &std::path::Path) -> Result<(), String> {
    let invalid = || "临时关闭目录路径或安全属性无效".to_string();
    let program_data = program_data_directory().ok_or_else(invalid)?;
    if !no_reparse_ancestors(&program_data) || !no_reparse_ancestors(worker_dir) {
        return Err(invalid());
    }
    let (Ok(program_data), Ok(worker_dir)) =
        (program_data.canonicalize(), worker_dir.canonicalize())
    else {
        return Err(invalid());
    };
    if worker_dir.parent() != Some(program_data.as_path())
        || !worker_dir
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("AgentsPads-disable-"))
            .is_some_and(valid_disable_worker_uuid)
    {
        return Err(invalid());
    }
    validate_admin_system_acl(&worker_dir, true)
}

#[cfg(windows)]
fn validate_registered_disable_worker_contents(worker_dir: &std::path::Path) -> Result<(), String> {
    for entry in std::fs::read_dir(worker_dir).map_err(|_| "无法读取待清理临时目录".to_string())?
    {
        let entry = entry.map_err(|_| "无法读取待清理临时目录项".to_string())?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let allowed = matches!(
            name.as_ref(),
            "agentspads-disable-worker.exe"
                | DISABLE_CLEANUP_READY_FILE
                | DISABLE_CLEANUP_REGISTRATION_FILE
                | DISABLE_STAGE_STATUS_FILE
                | DISABLE_STAGE_ACK_FILE
                | DISABLE_STAGE_CANCEL_FILE
                | DISABLE_STAGE_ACKED_FILE
                | DISABLE_STAGE_FINAL_FILE
        ) || (name.starts_with("stage-status-") && name.ends_with(".tmp"));
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|_| "无法验证待清理临时目录项".to_string())?;
        if !allowed
            || !file_type.is_file()
            || file_type.is_symlink()
            || !no_reparse_ancestors(&path)
        {
            return Err("待清理临时目录包含未验证的文件".to_string());
        }
        validate_admin_system_acl(&path, false)?;
    }
    Ok(())
}

#[cfg(windows)]
fn cleanup_failed_staged_worker(
    worker_dir: &std::path::Path,
    worker_exe: &std::path::Path,
    worker_identity: Option<(u32, u64)>,
) -> Result<(), String> {
    match std::fs::symlink_metadata(worker_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("无法确认本次临时目录是否仍存在".to_string()),
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err("本次临时目录路径已变化，保留未处理".to_string())
        }
        Ok(_) => {}
    }
    validate_disable_worker_directory(worker_dir)?;
    if let Some(registration) = read_disable_cleanup_registration(worker_dir)? {
        let Some((worker_pid, worker_created)) = worker_identity else {
            return Err("未启动关闭程序却发现清理身份记录，临时目录保留".to_string());
        };
        if registration.worker_pid != worker_pid || registration.worker_created != worker_created {
            return Err("临时目录清理身份与本次关闭程序不匹配，临时目录保留".to_string());
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match cleanup_process_observation(
                registration.cleanup_pid,
                registration.cleanup_created,
            ) {
                CleanupProcessObservation::Exited(_) => break,
                CleanupProcessObservation::Running if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                CleanupProcessObservation::Running => {
                    return Err("清理程序仍在运行，本次临时目录保留".to_string())
                }
                CleanupProcessObservation::Unknown => {
                    return Err("无法验证清理程序状态，本次临时目录保留".to_string())
                }
            }
        }
    }
    validate_disable_worker_directory(worker_dir)?;
    validate_registered_disable_worker_contents(worker_dir)?;
    let exe_metadata = match std::fs::symlink_metadata(worker_exe) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Some(metadata),
        Ok(_) => return Err("本次临时目录程序路径已变化，保留未处理".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err("无法验证本次临时目录程序，保留未处理".to_string()),
    };
    if exe_metadata.is_some() {
        validate_disable_worker_paths(worker_dir, worker_exe)?;
    }
    std::fs::remove_dir_all(worker_dir).map_err(|_| "无法回收本次已验证的临时目录".to_string())?;
    match std::fs::symlink_metadata(worker_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err("本次临时目录仍然存在".to_string()),
    }
}

#[cfg(any(windows, test))]
fn valid_disable_worker_boundary(
    program_data: &std::path::Path,
    worker_dir: &std::path::Path,
    worker_exe: &std::path::Path,
) -> bool {
    let (Ok(program_data), Ok(worker_dir), Ok(worker_exe)) = (
        program_data.canonicalize(),
        worker_dir.canonicalize(),
        worker_exe.canonicalize(),
    ) else {
        return false;
    };
    let Some(worker_parent) = worker_dir.parent() else {
        return false;
    };
    if !worker_parent
        .to_string_lossy()
        .eq_ignore_ascii_case(&program_data.to_string_lossy())
    {
        return false;
    }
    let Some(name) = worker_dir.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let Some(suffix) = name.strip_prefix("AgentsPads-disable-") else {
        return false;
    };
    let Ok(uuid) = uuid::Uuid::parse_str(suffix) else {
        return false;
    };
    uuid.to_string() == suffix.to_ascii_lowercase()
        && worker_exe.parent() == Some(worker_dir.as_path())
        && worker_exe.file_name().is_some_and(|value| {
            value.eq_ignore_ascii_case(std::ffi::OsStr::new("agentspads-disable-worker.exe"))
        })
        && worker_exe.is_file()
}

#[cfg(windows)]
fn program_data_directory() -> Option<std::path::PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_ProgramData, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    unsafe {
        let text = SHGetKnownFolderPath(&FOLDERID_ProgramData, KF_FLAG_DEFAULT, None).ok()?;
        let path = text.to_string().ok().map(std::path::PathBuf::from);
        CoTaskMemFree(Some(text.as_ptr().cast()));
        path
    }
}

#[cfg(windows)]
#[derive(Debug, PartialEq, Eq)]
enum DisableWorkerFailure {
    ReportedToStage,
    CleanupBlocked,
    NeedsDialog(String),
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DisableCleanupRegistration {
    worker_pid: u32,
    worker_created: u64,
    cleanup_pid: u32,
    cleanup_created: u64,
}

#[cfg(any(windows, test))]
fn encode_disable_cleanup_registration(
    registration: DisableCleanupRegistration,
    prefix: &str,
) -> String {
    format!(
        "{prefix} {} {} {} {}\n",
        registration.worker_pid,
        registration.worker_created,
        registration.cleanup_pid,
        registration.cleanup_created
    )
}

#[cfg(any(windows, test))]
fn parse_disable_cleanup_registration(
    text: &str,
    expected_prefix: &str,
) -> Result<DisableCleanupRegistration, String> {
    let mut fields = text.split_ascii_whitespace();
    if fields.next() != Some(expected_prefix) {
        return Err("清理进程身份记录无效".to_string());
    }
    let parse = |value: Option<&str>| value.and_then(|value| value.parse::<u64>().ok());
    let worker_pid = parse(fields.next())
        .filter(|pid| *pid > 0 && *pid <= u32::MAX as u64)
        .ok_or_else(|| "清理进程身份记录无效".to_string())? as u32;
    let worker_created = parse(fields.next())
        .filter(|created| *created > 0)
        .ok_or_else(|| "清理进程身份记录无效".to_string())?;
    let cleanup_pid = parse(fields.next())
        .filter(|pid| *pid > 0 && *pid <= u32::MAX as u64)
        .ok_or_else(|| "清理进程身份记录无效".to_string())? as u32;
    let cleanup_created = parse(fields.next())
        .filter(|created| *created > 0)
        .ok_or_else(|| "清理进程身份记录无效".to_string())?;
    if fields.next().is_some() {
        return Err("清理进程身份记录无效".to_string());
    }
    Ok(DisableCleanupRegistration {
        worker_pid,
        worker_created,
        cleanup_pid,
        cleanup_created,
    })
}

#[cfg(any(windows, test))]
fn valid_disable_worker_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|uuid| uuid.to_string() == value)
}

#[cfg(any(windows, test))]
fn cleanup_executor_can_commit(ready: bool, alive: bool) -> bool {
    ready && alive
}

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
enum DisableStageState {
    Ready,
    Failed(String),
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DisableStageDecision {
    Acknowledge,
    Cancel,
    Acknowledged,
    Commit,
    Abort,
}

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
struct DisableStageDecisionRecord {
    decision: DisableStageDecision,
    parent_pid: u32,
    parent_created: u64,
    worker_pid: u32,
}

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
struct DisableStageRecord {
    state: DisableStageState,
    parent_pid: u32,
    parent_created: u64,
    worker_pid: u32,
}

#[cfg(any(windows, test))]
const DISABLE_STAGE_STATUS_FILE: &str = "stage-status.txt";
#[cfg(any(windows, test))]
const DISABLE_STAGE_STATUS_MAX: usize = 8192;
#[cfg(any(windows, test))]
const DISABLE_STAGE_DECISION_MAX: usize = 256;
#[cfg(windows)]
const DISABLE_STAGE_ACK_FILE: &str = "stage-ack.txt";
#[cfg(windows)]
const DISABLE_STAGE_CANCEL_FILE: &str = "stage-cancel.txt";
#[cfg(windows)]
const DISABLE_STAGE_ACKED_FILE: &str = "stage-acked.txt";
#[cfg(windows)]
const DISABLE_STAGE_FINAL_FILE: &str = "stage-final.txt";

#[cfg(any(windows, test))]
fn encode_disable_stage_status(
    state: &DisableStageState,
    parent_pid: u32,
    parent_created: u64,
    worker_pid: u32,
) -> String {
    match state {
        DisableStageState::Ready => format!("READY {parent_pid} {parent_created} {worker_pid}\n"),
        DisableStageState::Failed(reason) => {
            format!("ERROR {parent_pid} {parent_created} {worker_pid}\n{reason}")
        }
    }
}

#[cfg(any(windows, test))]
fn encode_disable_stage_decision(
    decision: &DisableStageDecision,
    parent_pid: u32,
    parent_created: u64,
    worker_pid: u32,
) -> String {
    let state = match decision {
        DisableStageDecision::Acknowledge => "ACK",
        DisableStageDecision::Cancel => "CANCEL",
        DisableStageDecision::Acknowledged => "ACKED",
        DisableStageDecision::Commit => "COMMIT",
        DisableStageDecision::Abort => "ABORT",
    };
    format!("{state} {parent_pid} {parent_created} {worker_pid}\n")
}

#[cfg(any(windows, test))]
fn parse_disable_stage_decision(bytes: &[u8]) -> Result<DisableStageDecisionRecord, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "临时关闭程序确认编码无效".to_string())?;
    if text.len() > DISABLE_STAGE_DECISION_MAX {
        return Err("临时关闭程序确认内容过长".to_string());
    }
    let mut fields = text.split_ascii_whitespace();
    let decision = match fields.next() {
        Some("ACK") => DisableStageDecision::Acknowledge,
        Some("CANCEL") => DisableStageDecision::Cancel,
        Some("ACKED") => DisableStageDecision::Acknowledged,
        Some("COMMIT") => DisableStageDecision::Commit,
        Some("ABORT") => DisableStageDecision::Abort,
        _ => return Err("临时关闭程序确认内容无效".to_string()),
    };
    let parent_pid = fields
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value != 0);
    let parent_created = fields
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value != 0);
    let worker_pid = fields
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value != 0);
    if fields.next().is_some() {
        return Err("临时关闭程序确认格式无效".to_string());
    }
    let (Some(parent_pid), Some(parent_created), Some(worker_pid)) =
        (parent_pid, parent_created, worker_pid)
    else {
        return Err("临时关闭程序确认身份无效".to_string());
    };
    Ok(DisableStageDecisionRecord {
        decision,
        parent_pid,
        parent_created,
        worker_pid,
    })
}

#[cfg(any(windows, test))]
fn validate_disable_stage_decision(
    record: DisableStageDecisionRecord,
    parent_pid: u32,
    parent_created: u64,
    worker_pid: u32,
    expected: DisableStageDecision,
) -> Result<(), String> {
    if record.parent_pid != parent_pid
        || record.parent_created != parent_created
        || record.worker_pid != worker_pid
        || record.decision != expected
    {
        return Err("临时关闭程序确认与本次进程不匹配".to_string());
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn parse_disable_stage_status(bytes: &[u8]) -> Result<DisableStageRecord, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "临时关闭程序状态编码无效".to_string())?;
    if text.len() > DISABLE_STAGE_STATUS_MAX {
        return Err("临时关闭程序状态过长".to_string());
    }
    let (header, detail) = text
        .split_once('\n')
        .map_or((text.trim_end_matches('\r'), ""), |(header, detail)| {
            (header.trim_end_matches('\r'), detail)
        });
    let mut fields = header.split_ascii_whitespace();
    let state = fields.next();
    let parent_pid = fields
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value != 0);
    let parent_created = fields
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value != 0);
    let worker_pid = fields
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value != 0);
    if fields.next().is_some() {
        return Err("临时关闭程序状态格式无效".to_string());
    }
    let (Some(parent_pid), Some(parent_created), Some(worker_pid)) =
        (parent_pid, parent_created, worker_pid)
    else {
        return Err("临时关闭程序状态身份无效".to_string());
    };
    let state = match state {
        Some("READY") if detail.is_empty() => DisableStageState::Ready,
        Some("ERROR") if !detail.trim().is_empty() => DisableStageState::Failed(detail.to_string()),
        _ => return Err("临时关闭程序状态内容无效".to_string()),
    };
    Ok(DisableStageRecord {
        state,
        parent_pid,
        parent_created,
        worker_pid,
    })
}

#[cfg(any(windows, test))]
fn validate_disable_stage_record(
    record: DisableStageRecord,
    parent_pid: u32,
    parent_created: u64,
    worker_pid: u32,
) -> Result<(), String> {
    if record.parent_pid != parent_pid
        || record.parent_created != parent_created
        || record.worker_pid != worker_pid
    {
        return Err("临时关闭程序状态与本次进程不匹配".to_string());
    }
    match record.state {
        DisableStageState::Ready => Ok(()),
        DisableStageState::Failed(reason) => Err(reason),
    }
}

#[cfg(windows)]
fn write_disable_stage_status(
    worker_dir: &std::path::Path,
    state: &DisableStageState,
    parent_pid: u32,
    parent_created: u64,
    worker_pid: u32,
) -> Result<(), String> {
    use std::io::Write;
    let data = encode_disable_stage_status(state, parent_pid, parent_created, worker_pid);
    if data.len() > DISABLE_STAGE_STATUS_MAX {
        return Err("临时关闭程序状态过长".to_string());
    }
    let status = worker_dir.join(DISABLE_STAGE_STATUS_FILE);
    let temp = worker_dir.join(format!(
        "stage-status-{}-{worker_pid}.tmp",
        uuid::Uuid::new_v4()
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|_| "无法写入临时关闭程序状态".to_string())?;
    set_administrators_owner(&temp).map_err(|_| "无法保护临时关闭程序状态".to_string())?;
    validate_admin_system_acl(&temp, false)?;
    file.write_all(data.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|_| "无法写入临时关闭程序状态".to_string())?;
    drop(file);
    if !move_file_without_replacing(&temp, &status) {
        let _ = std::fs::remove_file(&temp);
        return Err("无法发布临时关闭程序状态".to_string());
    }
    Ok(())
}

#[cfg(windows)]
fn move_file_without_replacing(source: &std::path::Path, destination: &std::path::Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileW(existing: *const u16, new: *const u16) -> i32;
    }
    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe { MoveFileW(source_wide.as_ptr(), destination_wide.as_ptr()) != 0 }
}

#[cfg(windows)]
fn read_disable_stage_status(
    worker_dir: &std::path::Path,
    parent_pid: u32,
    parent_created: u64,
    worker_pid: u32,
) -> Result<Option<Result<(), String>>, String> {
    use std::io::Read;
    let path = worker_dir.join(DISABLE_STAGE_STATUS_FILE);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("无法读取临时关闭程序状态".to_string()),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > DISABLE_STAGE_STATUS_MAX as u64
        || !no_reparse_ancestors(&path)
    {
        return Err("临时关闭程序状态路径无效".to_string());
    }
    validate_admin_system_acl(&path, false)?;
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .and_then(|file| {
            file.take(DISABLE_STAGE_STATUS_MAX as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|_| "无法读取临时关闭程序状态".to_string())?;
    let record = parse_disable_stage_status(&bytes)?;
    Ok(Some(validate_disable_stage_record(
        record,
        parent_pid,
        parent_created,
        worker_pid,
    )))
}

#[cfg(windows)]
fn write_disable_stage_decision(
    worker_dir: &std::path::Path,
    decision: DisableStageDecision,
    parent_pid: u32,
    parent_created: u64,
    worker_pid: u32,
) -> Result<(), String> {
    use std::io::Write;
    let (filename, label) = match decision {
        DisableStageDecision::Acknowledge => (DISABLE_STAGE_ACK_FILE, "确认"),
        DisableStageDecision::Cancel => (DISABLE_STAGE_CANCEL_FILE, "取消标记"),
        DisableStageDecision::Acknowledged => (DISABLE_STAGE_ACKED_FILE, "确认回执"),
        DisableStageDecision::Commit => (DISABLE_STAGE_FINAL_FILE, "最终确认"),
        DisableStageDecision::Abort => (DISABLE_STAGE_FINAL_FILE, "取消确认"),
    };
    let data = encode_disable_stage_decision(&decision, parent_pid, parent_created, worker_pid);
    let temp = worker_dir.join(format!(
        "{filename}-{}-{worker_pid}.tmp",
        uuid::Uuid::new_v4()
    ));
    let destination = worker_dir.join(filename);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|_| format!("无法写入临时关闭程序{label}"))?;
    set_administrators_owner(&temp).map_err(|_| format!("无法保护临时关闭程序{label}"))?;
    validate_admin_system_acl(&temp, false)?;
    file.write_all(data.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|_| format!("无法写入临时关闭程序{label}"))?;
    drop(file);
    if !move_file_without_replacing(&temp, &destination) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("无法发布临时关闭程序{label}"));
    }
    Ok(())
}

#[cfg(windows)]
fn read_disable_stage_decision_record_file(
    worker_dir: &std::path::Path,
    filename: &str,
    parent_pid: u32,
    parent_created: u64,
    worker_pid: u32,
) -> Result<Option<DisableStageDecisionRecord>, String> {
    use std::io::Read;
    let path = worker_dir.join(filename);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("无法读取临时关闭程序交接标记".to_string()),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > DISABLE_STAGE_DECISION_MAX as u64
        || !no_reparse_ancestors(&path)
    {
        return Err("临时关闭程序交接标记路径无效".to_string());
    }
    validate_admin_system_acl(&path, false)?;
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .and_then(|file| {
            file.take(DISABLE_STAGE_DECISION_MAX as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|_| "无法读取临时关闭程序交接标记".to_string())?;
    let record = parse_disable_stage_decision(&bytes)?;
    if record.parent_pid != parent_pid
        || record.parent_created != parent_created
        || record.worker_pid != worker_pid
    {
        return Err("临时关闭程序交接标记与本次进程不匹配".to_string());
    }
    Ok(Some(record))
}

#[cfg(windows)]
fn read_disable_stage_decision_file(
    worker_dir: &std::path::Path,
    filename: &str,
    parent_pid: u32,
    parent_created: u64,
    worker_pid: u32,
    expected: DisableStageDecision,
) -> Result<Option<DisableStageDecision>, String> {
    let Some(record) = read_disable_stage_decision_record_file(
        worker_dir,
        filename,
        parent_pid,
        parent_created,
        worker_pid,
    )?
    else {
        return Ok(None);
    };
    validate_disable_stage_decision(record, parent_pid, parent_created, worker_pid, expected)?;
    Ok(Some(expected))
}

#[cfg(windows)]
fn read_disable_stage_decision(
    worker_dir: &std::path::Path,
    parent_pid: u32,
    parent_created: u64,
    worker_pid: u32,
) -> Result<Option<DisableStageDecision>, String> {
    for (filename, expected) in [
        (DISABLE_STAGE_CANCEL_FILE, DisableStageDecision::Cancel),
        (DISABLE_STAGE_ACK_FILE, DisableStageDecision::Acknowledge),
    ] {
        if let Some(decision) = read_disable_stage_decision_file(
            worker_dir,
            filename,
            parent_pid,
            parent_created,
            worker_pid,
            expected,
        )? {
            return Ok(Some(decision));
        }
    }
    Ok(None)
}

#[cfg(windows)]
fn cancel_disable_worker_stage(
    worker_dir: &std::path::Path,
    parent_pid: u32,
    parent_created: u64,
    worker_pid: u32,
    reason: &str,
) -> String {
    let cancel_result = write_disable_stage_decision(
        worker_dir,
        DisableStageDecision::Cancel,
        parent_pid,
        parent_created,
        worker_pid,
    );
    let abort_result = write_disable_stage_decision(
        worker_dir,
        DisableStageDecision::Abort,
        parent_pid,
        parent_created,
        worker_pid,
    );
    match (cancel_result, abort_result) {
        (Ok(()), Ok(())) => {
            format!("{reason}；交接已取消，管理员安装保持不变。")
        }
        (cancel, abort) => format!(
            "{reason}；管理员安装保持不变。取消标记：{}；最终取消标记：{}",
            cancel.err().unwrap_or_else(|| "已发布".to_string()),
            abort.err().unwrap_or_else(|| "已发布".to_string()),
        ),
    }
}

#[cfg(windows)]
fn disable_worker_exit_reason(status: &std::process::ExitStatus, phase: &str) -> String {
    if status.code() == Some(STAGE_HELPER_CLEANUP_BLOCKED_EXIT as i32) {
        cleanup_blocked_reason("临时目录清理程序在交接过程中退出")
    } else {
        format!("临时关闭程序在{phase}退出（退出码 {:?}）", status.code())
    }
}

#[cfg(windows)]
fn wait_for_disable_worker_stage(
    worker: &mut std::process::Child,
    worker_dir: &std::path::Path,
    parent_pid: u32,
    parent_created: u64,
) -> Result<(), String> {
    let worker_pid = worker.id();
    let deadline = std::time::Instant::now() + DISABLE_STAGE_WAIT;
    loop {
        match read_disable_stage_status(worker_dir, parent_pid, parent_created, worker_pid) {
            Ok(Some(Ok(()))) => match worker.try_wait() {
                Ok(None) => {
                    return match write_disable_stage_decision(
                        worker_dir,
                        DisableStageDecision::Acknowledge,
                        parent_pid,
                        parent_created,
                        worker_pid,
                    ) {
                        Ok(()) => wait_for_disable_worker_acknowledged(
                            worker,
                            worker_dir,
                            parent_pid,
                            parent_created,
                            worker_pid,
                        ),
                        Err(reason) => Err(cancel_disable_worker_stage(
                            worker_dir,
                            parent_pid,
                            parent_created,
                            worker_pid,
                            &format!("无法确认普通程序已准备就绪：{reason}"),
                        )),
                    };
                }
                Ok(Some(status)) => {
                    return Err(cancel_disable_worker_stage(
                        worker_dir,
                        parent_pid,
                        parent_created,
                        worker_pid,
                        &disable_worker_exit_reason(&status, "交接确认前"),
                    ));
                }
                Err(_) => {
                    return Err(cancel_disable_worker_stage(
                        worker_dir,
                        parent_pid,
                        parent_created,
                        worker_pid,
                        "无法确认临时关闭程序仍在等待交接",
                    ));
                }
            },
            Ok(Some(Err(reason))) => {
                return Err(cancel_disable_worker_stage(
                    worker_dir,
                    parent_pid,
                    parent_created,
                    worker_pid,
                    &reason,
                ));
            }
            Ok(None) => {}
            Err(reason) => {
                return Err(cancel_disable_worker_stage(
                    worker_dir,
                    parent_pid,
                    parent_created,
                    worker_pid,
                    &reason,
                ));
            }
        }
        match worker.try_wait() {
            Ok(Some(status)) => {
                return Err(cancel_disable_worker_stage(
                    worker_dir,
                    parent_pid,
                    parent_created,
                    worker_pid,
                    &disable_worker_exit_reason(&status, "报告状态前"),
                ));
            }
            Ok(None) => {}
            Err(_) => {
                return Err(cancel_disable_worker_stage(
                    worker_dir,
                    parent_pid,
                    parent_created,
                    worker_pid,
                    "无法确认临时关闭程序状态",
                ));
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(cancel_disable_worker_stage(
                worker_dir,
                parent_pid,
                parent_created,
                worker_pid,
                "关闭管理员模式准备超时",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(windows)]
fn wait_for_disable_worker_acknowledged(
    worker: &mut std::process::Child,
    worker_dir: &std::path::Path,
    parent_pid: u32,
    parent_created: u64,
    worker_pid: u32,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + DISABLE_STAGE_WAIT;
    loop {
        match read_disable_stage_decision_file(
            worker_dir,
            DISABLE_STAGE_ACKED_FILE,
            parent_pid,
            parent_created,
            worker_pid,
            DisableStageDecision::Acknowledged,
        ) {
            Ok(Some(_)) => match worker.try_wait() {
                Ok(None) => {
                    return match write_disable_stage_decision(
                        worker_dir,
                        DisableStageDecision::Commit,
                        parent_pid,
                        parent_created,
                        worker_pid,
                    ) {
                        Ok(()) => Ok(()),
                        Err(reason) => Err(cancel_disable_worker_stage(
                            worker_dir,
                            parent_pid,
                            parent_created,
                            worker_pid,
                            &format!("无法确认关闭交接：{reason}"),
                        )),
                    };
                }
                Ok(Some(status)) => {
                    return Err(cancel_disable_worker_stage(
                        worker_dir,
                        parent_pid,
                        parent_created,
                        worker_pid,
                        &disable_worker_exit_reason(&status, "确认交接前"),
                    ));
                }
                Err(_) => {
                    return Err(cancel_disable_worker_stage(
                        worker_dir,
                        parent_pid,
                        parent_created,
                        worker_pid,
                        "无法确认临时关闭程序仍在等待交接",
                    ));
                }
            },
            Ok(None) => {}
            Err(reason) => {
                return Err(cancel_disable_worker_stage(
                    worker_dir,
                    parent_pid,
                    parent_created,
                    worker_pid,
                    &reason,
                ));
            }
        }
        match worker.try_wait() {
            Ok(Some(status)) => {
                return Err(cancel_disable_worker_stage(
                    worker_dir,
                    parent_pid,
                    parent_created,
                    worker_pid,
                    &disable_worker_exit_reason(&status, "确认交接前"),
                ));
            }
            Ok(None) => {}
            Err(_) => {
                return Err(cancel_disable_worker_stage(
                    worker_dir,
                    parent_pid,
                    parent_created,
                    worker_pid,
                    "无法确认临时关闭程序状态",
                ));
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(cancel_disable_worker_stage(
                worker_dir,
                parent_pid,
                parent_created,
                worker_pid,
                "等待临时关闭程序确认交接超时",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(any(windows, test))]
fn authorize_disable_stage_unload(
    ready: Result<(), String>,
    acknowledgement: Option<DisableStageDecision>,
    cancellation: Option<DisableStageDecision>,
) -> Result<(), String> {
    match cancellation {
        Some(DisableStageDecision::Cancel) => {
            return Err("父进程取消了关闭交接；管理员安装未卸载。".to_string())
        }
        Some(DisableStageDecision::Acknowledge)
        | Some(DisableStageDecision::Acknowledged)
        | Some(DisableStageDecision::Commit)
        | Some(DisableStageDecision::Abort) => {
            return Err("临时关闭程序取消标记无效；管理员安装未卸载。".to_string())
        }
        None => {}
    }
    ready?;
    match acknowledgement {
        Some(DisableStageDecision::Acknowledge) => Ok(()),
        Some(DisableStageDecision::Cancel)
        | Some(DisableStageDecision::Acknowledged)
        | Some(DisableStageDecision::Commit)
        | Some(DisableStageDecision::Abort) => {
            Err("临时关闭程序确认标记无效；管理员安装未卸载。".to_string())
        }
        None => Err("父进程尚未确认普通程序；管理员安装未卸载。".to_string()),
    }
}

#[cfg(any(windows, test))]
fn authorize_disable_stage_commit(
    acknowledged: Option<DisableStageDecision>,
    final_decision: Option<DisableStageDecision>,
) -> Result<(), String> {
    match acknowledged {
        Some(DisableStageDecision::Acknowledged) => {}
        _ => return Err("临时关闭程序尚未确认交接；管理员安装未卸载。".to_string()),
    }
    match final_decision {
        Some(DisableStageDecision::Commit) => Ok(()),
        Some(DisableStageDecision::Abort) => {
            Err("父进程取消了关闭交接；管理员安装未卸载。".to_string())
        }
        _ => Err("父进程尚未提交关闭交接；管理员安装未卸载。".to_string()),
    }
}

#[cfg(windows)]
fn wait_for_disable_worker_decision(
    cleanup: &mut DisableCleanupExecutor,
    worker_dir: &std::path::Path,
    parent_pid: u32,
    parent_created: u64,
    worker_pid: u32,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + DISABLE_STAGE_WAIT;
    loop {
        ensure_cleanup_executor_alive(cleanup)?;
        match read_disable_stage_decision(worker_dir, parent_pid, parent_created, worker_pid)? {
            Some(DisableStageDecision::Acknowledge) => {
                let ready =
                    read_disable_stage_status(worker_dir, parent_pid, parent_created, worker_pid)?
                        .ok_or_else(|| "临时关闭程序尚未发布 READY 状态。".to_string())?;
                return authorize_disable_stage_unload(
                    ready,
                    Some(DisableStageDecision::Acknowledge),
                    None,
                );
            }
            Some(DisableStageDecision::Cancel) => {
                return authorize_disable_stage_unload(
                    Ok(()),
                    None,
                    Some(DisableStageDecision::Cancel),
                );
            }
            Some(_) => return Err("临时关闭程序交接标记状态无效。".to_string()),
            None => {}
        }
        if std::time::Instant::now() >= deadline {
            return Err("等待父进程确认普通程序超时；管理员安装未卸载。".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
#[cfg(windows)]
static ACTIVE_DISABLE_WORKER_MUTEX: std::sync::OnceLock<std::os::windows::io::OwnedHandle> =
    std::sync::OnceLock::new();

#[cfg(windows)]
fn acquire_disable_worker_mutex() -> Result<(), String> {
    use std::os::windows::io::FromRawHandle;
    #[repr(C)]
    struct SecurityAttributes {
        length: u32,
        descriptor: *mut std::ffi::c_void,
        inherit_handle: i32,
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            string: *const u16,
            revision: u32,
            descriptor: *mut *mut std::ffi::c_void,
            size: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateMutexW(
            attributes: *mut SecurityAttributes,
            initial_owner: i32,
            name: *const u16,
        ) -> *mut std::ffi::c_void;
        fn GetLastError() -> u32;
        fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        fn SetLastError(error: u32);
        fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
    }
    if ACTIVE_DISABLE_WORKER_MUTEX.get().is_some() {
        return Err("当前进程已持有关闭流程锁".to_string());
    }
    let sddl: Vec<u16> = "O:BAG:BAD:P(A;;GA;;;BA)(A;;GA;;;SY)"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = std::ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if converted == 0 || descriptor.is_null() {
        return Err("无法创建关闭流程互斥锁安全描述符".to_string());
    }
    let mut attributes = SecurityAttributes {
        length: std::mem::size_of::<SecurityAttributes>() as u32,
        descriptor,
        inherit_handle: 0,
    };
    let name: Vec<u16> = r"Local\AgentsPadsDisableWorker-v1"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe { SetLastError(0) };
    let handle = unsafe { CreateMutexW(&mut attributes, 0, name.as_ptr()) };
    let last_error = unsafe { GetLastError() };
    unsafe {
        let _ = LocalFree(descriptor);
    }
    if handle.is_null() {
        return Err(format!(
            "无法创建关闭流程互斥锁（Win32 错误码 {last_error}）：{}",
            std::io::Error::from_raw_os_error(last_error as i32)
        ));
    }
    let owned = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle) };
    if last_error == 183 {
        return Err("已有另一个管理员关闭流程正在运行；管理员模式保持开启。".to_string());
    }
    let wait = unsafe { WaitForSingleObject(handle, 0) };
    if wait != 0 && wait != 0x80 {
        return Err(format!(
            "无法取得关闭流程互斥锁（等待结果 0x{wait:08X}）；管理员模式保持开启。"
        ));
    }
    ACTIVE_DISABLE_WORKER_MUTEX
        .set(owned)
        .map_err(|_| "当前进程已持有关闭流程锁".to_string())
}

#[cfg(windows)]
fn run_disable_worker(parent_pid: u32, parent_created: u64) -> Result<(), DisableWorkerFailure> {
    let worker_exe = std::env::current_exe().map_err(|_| DisableWorkerFailure::ReportedToStage)?;
    let worker_dir = worker_exe
        .parent()
        .ok_or(DisableWorkerFailure::ReportedToStage)?;
    let worker_pid = std::process::id();
    let mut ready_published = false;
    let mut handoff_acknowledged = false;
    let result = (|| -> Result<(), String> {
        if !is_elevated() {
            return Err("临时关闭程序未获得管理员权限".to_string());
        }
        validate_disable_worker_paths(worker_dir, &worker_exe)?;
        let worker_created = current_process_creation_time()?;
        let mut cleanup =
            schedule_worker_directory_cleanup(worker_dir, worker_pid, worker_created)?;
        acquire_disable_worker_mutex()?;
        let (root, protected_exe) =
            protected_target().ok_or_else(|| "无法验证管理员安装目录".to_string())?;
        let ordinary_exe = ordinary_source_exe(&root, &protected_exe)?;
        let child = create_suspended_medium_child(&ordinary_exe)?;
        ensure_cleanup_executor_alive(&mut cleanup)?;
        write_disable_stage_status(
            worker_dir,
            &DisableStageState::Ready,
            parent_pid,
            parent_created,
            worker_pid,
        )?;
        ready_published = true;
        wait_for_disable_worker_decision(
            &mut cleanup,
            worker_dir,
            parent_pid,
            parent_created,
            worker_pid,
        )?;
        ensure_cleanup_executor_alive(&mut cleanup)?;
        write_disable_stage_decision(
            worker_dir,
            DisableStageDecision::Acknowledged,
            parent_pid,
            parent_created,
            worker_pid,
        )?;
        wait_for_disable_worker_commit(
            &mut cleanup,
            worker_dir,
            parent_pid,
            parent_created,
            worker_pid,
        )?;
        handoff_acknowledged = true;
        wait_for_pid_created(parent_pid, parent_created, DISABLE_WORKER_WAIT)
            .map_err(str::to_string)?;
        ensure_cleanup_executor_alive(&mut cleanup)?;
        delete_admin_tasks()?;
        ensure_cleanup_executor_alive(&mut cleanup)?;
        if root.file_name().and_then(|name| name.to_str()) != Some("AgentsPads") {
            return Err("管理员安装目录校验失败".to_string());
        }
        ensure_cleanup_executor_alive(&mut cleanup)?;
        std::fs::remove_dir_all(&root).map_err(|_| "无法删除受保护的管理员目录".to_string())?;
        if root.exists() {
            return Err("受保护的管理员目录仍然存在".to_string());
        }
        let child = child.resume()?;
        if let Err(reason) =
            wait_for_ordinary_process(&child, &ordinary_exe, std::time::Duration::from_secs(12))
        {
            child.terminate_and_wait();
            return Err(reason);
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(()),
        Err(reason) if !handoff_acknowledged && reason.contains(DISABLE_CLEANUP_BLOCKED_TAG) => {
            Err(DisableWorkerFailure::CleanupBlocked)
        }
        Err(reason) if handoff_acknowledged => Err(DisableWorkerFailure::NeedsDialog(reason)),
        Err(reason) if !ready_published => {
            let _ = write_disable_stage_status(
                worker_dir,
                &DisableStageState::Failed(reason),
                parent_pid,
                parent_created,
                worker_pid,
            );
            Err(DisableWorkerFailure::ReportedToStage)
        }
        Err(_) => Err(DisableWorkerFailure::ReportedToStage),
    }
}
#[cfg(windows)]
fn wait_for_disable_worker_commit(
    cleanup: &mut DisableCleanupExecutor,
    worker_dir: &std::path::Path,
    parent_pid: u32,
    parent_created: u64,
    worker_pid: u32,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + DISABLE_STAGE_WAIT;
    loop {
        ensure_cleanup_executor_alive(cleanup)?;
        if let Some(record) = read_disable_stage_decision_record_file(
            worker_dir,
            DISABLE_STAGE_FINAL_FILE,
            parent_pid,
            parent_created,
            worker_pid,
        )? {
            authorize_disable_stage_commit(
                Some(DisableStageDecision::Acknowledged),
                Some(record.decision),
            )?;
            ensure_cleanup_executor_alive(cleanup)?;
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err("等待父进程提交关闭交接超时；管理员安装未卸载。".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(windows)]
fn schedule_worker_directory_cleanup(
    worker_dir: &std::path::Path,
    worker_pid: u32,
    worker_created: u64,
) -> Result<DisableCleanupExecutor, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let powershell = system_directory()
        .ok_or_else(|| "无法确定系统 PowerShell 路径".to_string())?
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    if !powershell.is_file() || !no_reparse_ancestors(&powershell) {
        return Err("系统 PowerShell 不可用，未开始卸载".to_string());
    }
    let worker_exe = worker_dir.join("agentspads-disable-worker.exe");
    validate_disable_worker_paths(worker_dir, &worker_exe)?;
    let dir64 = encode_base64(&encode_os_wide(worker_dir.as_os_str()));
    let script = worker_cleanup_script(worker_pid, worker_created, &dir64);
    let encoded = encode_base64(
        &script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    create_disable_cleanup_ready_file(worker_dir)
        .map_err(|_| cleanup_blocked_reason("无法准备临时目录清理就绪记录"))?;
    let mut cleanup = std::process::Command::new(powershell)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-EncodedCommand",
            &encoded,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .current_dir(system_directory().ok_or_else(|| "无法确定系统目录".to_string())?)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| cleanup_blocked_reason("无法启动临时目录清理程序"))?;
    let registration = DisableCleanupRegistration {
        worker_pid,
        worker_created,
        cleanup_pid: cleanup.id(),
        cleanup_created: match process_creation_time(cleanup.id()) {
            Ok(created) => created,
            Err(_) => {
                stop_cleanup_executor(&mut cleanup);
                return Err(cleanup_blocked_reason("无法验证临时目录清理程序身份"));
            }
        },
    };
    if write_disable_cleanup_registration(worker_dir, registration).is_err() {
        stop_cleanup_executor(&mut cleanup);
        return Err(cleanup_blocked_reason("无法保存临时目录清理身份"));
    }
    if let Err(reason) = wait_for_cleanup_executor_ready(&mut cleanup, worker_dir, registration) {
        stop_cleanup_executor(&mut cleanup);
        return Err(reason);
    }
    Ok(DisableCleanupExecutor {
        child: cleanup,
        registration,
    })
}

#[cfg(windows)]
struct DisableCleanupExecutor {
    child: std::process::Child,
    registration: DisableCleanupRegistration,
}

#[cfg(windows)]
fn stop_cleanup_executor(cleanup: &mut std::process::Child) {
    let _ = cleanup.kill();
    let _ = cleanup.wait();
}

#[cfg(windows)]
fn ensure_cleanup_executor_alive(cleanup: &mut DisableCleanupExecutor) -> Result<(), String> {
    if cleanup.registration.cleanup_pid == 0
        || cleanup.registration.cleanup_created == 0
        || cleanup.child.id() != cleanup.registration.cleanup_pid
    {
        return Err(cleanup_blocked_reason("临时目录清理程序身份无效"));
    }
    match cleanup.child.try_wait() {
        Ok(None) => Ok(()),
        Ok(Some(status)) => Err(cleanup_blocked_reason(&format!(
            "临时目录清理程序在交接前退出（退出码 {:?}）",
            status.code()
        ))),
        Err(_) => Err(cleanup_blocked_reason("无法确认临时目录清理程序仍在运行")),
    }
}

#[cfg(windows)]
fn create_disable_cleanup_ready_file(worker_dir: &std::path::Path) -> Result<(), String> {
    let path = worker_dir.join(DISABLE_CLEANUP_READY_FILE);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|_| "无法创建临时目录清理就绪记录".to_string())?;
    drop(file);
    set_administrators_owner(&path).map_err(|_| "无法保护临时目录清理就绪记录".to_string())?;
    validate_admin_system_acl(&path, false)
}

#[cfg(any(windows, test))]
const DISABLE_CLEANUP_BLOCKED_TAG: &str = "DISABLE_CLEANUP_BLOCKED:";

#[cfg(any(windows, test))]
fn cleanup_blocked_reason(detail: &str) -> String {
    format!("{DISABLE_CLEANUP_BLOCKED_TAG} {detail}")
}

#[cfg(windows)]
fn wait_for_cleanup_executor_ready(
    cleanup: &mut std::process::Child,
    worker_dir: &std::path::Path,
    registration: DisableCleanupRegistration,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + DISABLE_CLEANUP_READY_WAIT;
    loop {
        match cleanup.try_wait() {
            Ok(Some(status)) => {
                return Err(cleanup_blocked_reason(&format!(
                    "临时目录清理程序在交接前退出（退出码 {:?}）",
                    status.code()
                )))
            }
            Ok(None) => {}
            Err(_) => return Err(cleanup_blocked_reason("无法确认临时目录清理程序仍在运行")),
        }
        let ready = match read_disable_cleanup_ready(worker_dir, registration) {
            Ok(ready) => ready,
            Err(reason) => return Err(cleanup_blocked_reason(&reason)),
        };
        if cleanup_executor_can_commit(ready, true) {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(cleanup_blocked_reason(
                "临时目录清理程序未能在时限内报告就绪",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(windows)]
fn write_disable_cleanup_registration(
    worker_dir: &std::path::Path,
    registration: DisableCleanupRegistration,
) -> Result<(), String> {
    use std::io::Write;
    let path = worker_dir.join(DISABLE_CLEANUP_REGISTRATION_FILE);
    let data = encode_disable_cleanup_registration(registration, "PENDING");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|_| "无法创建临时目录清理身份记录".to_string())?;
    set_administrators_owner(&path).map_err(|_| "无法保护临时目录清理身份记录".to_string())?;
    validate_admin_system_acl(&path, false)?;
    file.write_all(data.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|_| "无法写入临时目录清理身份记录".to_string())
}

#[cfg(windows)]
fn read_disable_cleanup_ready(
    worker_dir: &std::path::Path,
    expected: DisableCleanupRegistration,
) -> Result<bool, String> {
    let path = worker_dir.join(DISABLE_CLEANUP_READY_FILE);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err("无法读取临时目录清理就绪状态".to_string()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || !no_reparse_ancestors(&path) {
        return Err("临时目录清理就绪状态路径无效".to_string());
    }
    validate_admin_system_acl(&path, false)?;
    if metadata.len() > 256 {
        return Err("临时目录清理就绪状态过长".to_string());
    }
    if metadata.len() == 0 {
        return Ok(false);
    }
    let text =
        std::fs::read_to_string(path).map_err(|_| "无法读取临时目录清理就绪状态".to_string())?;
    let Ok(observed) = parse_disable_cleanup_registration(&text, "READY") else {
        return Ok(false);
    };
    if observed != expected {
        return Err("临时目录清理进程身份不匹配".to_string());
    }
    Ok(true)
}

#[cfg(any(windows, test))]
fn worker_cleanup_script(worker_pid: u32, worker_created: u64, dir64: &str) -> String {
    format!(
        "$ErrorActionPreference='Stop'; $d=[Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('{dir64}')); try {{ $p=Get-Process -Id {worker_pid} -ErrorAction Stop; $created=$p.StartTime.ToUniversalTime().ToFileTimeUtc(); if ($created -ne {worker_created}) {{ throw 'worker identity mismatch' }}; $self=Get-Process -Id $PID; $selfCreated=$self.StartTime.ToUniversalTime().ToFileTimeUtc(); [IO.File]::WriteAllText((Join-Path $d '{DISABLE_CLEANUP_READY_FILE}'), ('READY {worker_pid} {worker_created} '+$PID+' '+$selfCreated), [Text.Encoding]::ASCII); $p.WaitForExit(); Remove-Item -LiteralPath $d -Recurse -Force -ErrorAction Stop; if (Test-Path -LiteralPath $d) {{ throw 'worker directory remains' }}; exit 0 }} catch {{ if (Test-Path -LiteralPath $d) {{ try {{ (New-Object -ComObject WScript.Shell).Popup('AgentsPads 临时关闭目录清理失败。',0,'AgentsPads',48) | Out-Null }} catch {{}} }}; exit 1 }}"
    )
}

#[cfg(windows)]
fn encode_os_wide(path: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.encode_wide().flat_map(u16::to_le_bytes).collect()
}

#[cfg(any(windows, test))]
fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        out.push(TABLE[(a >> 2) as usize] as char);
        out.push(TABLE[(((a & 0x03) << 4) | (b >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(((b & 0x0f) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(c & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(windows)]
fn delete_admin_tasks() -> Result<(), String> {
    use windows::core::{BSTR, HRESULT};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED,
    };
    use windows::Win32::System::TaskScheduler::{ITaskService, TaskScheduler};
    use windows::Win32::System::Variant::VARIANT;
    struct ComApartment;
    impl Drop for ComApartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() }
        }
    }
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_MULTITHREADED);
        if initialized.is_err() {
            return Err("无法连接计划任务服务".to_string());
        }
        let _apartment = ComApartment;
        let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)
            .map_err(|_| "无法连接计划任务服务".to_string())?;
        let empty = VARIANT::default();
        service
            .Connect(&empty, &empty, &empty, &empty)
            .map_err(|_| "无法连接计划任务服务".to_string())?;
        let folder = service
            .GetFolder(&BSTR::from("\\"))
            .map_err(|_| "无法读取管理员计划任务".to_string())?;
        let missing = HRESULT(0x8007_0002u32 as i32);
        for name in [TASK_NAME, AUTOSTART_TASK_NAME] {
            let name = BSTR::from(name);
            match folder.GetTask(&name) {
                Ok(_) => folder
                    .DeleteTask(&name, 0)
                    .map_err(|_| "无法删除管理员计划任务".to_string())?,
                Err(error) if error.code() == missing => {}
                Err(_) => return Err("无法确认管理员计划任务状态".to_string()),
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn show_disable_worker_failure(reason: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let text: Vec<u16> = format!("关闭管理员模式失败：{reason}")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            windows::core::w!("AgentsPads"),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(windows)]
fn system_directory() -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut buffer = vec![0u16; 32768];
    let len = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    if len == 0 || len >= buffer.len() {
        return None;
    }
    Some(std::path::PathBuf::from(std::ffi::OsString::from_wide(
        &buffer[..len],
    )))
}

#[cfg(windows)]
fn install_copy(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    if same_file(src, dest) {
        return Ok(());
    }
    match std::fs::remove_file(dest) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut input = std::fs::File::open(src)?;
    let mut output = std::fs::File::create(dest)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()
}

#[cfg(windows)]
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

#[cfg(windows)]
fn protected_target() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{
        FOLDERID_ProgramFilesX64, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
    };
    let root = unsafe {
        let text = SHGetKnownFolderPath(&FOLDERID_ProgramFilesX64, KF_FLAG_DEFAULT, None).ok()?;
        let root = text.to_string().ok();
        CoTaskMemFree(Some(text.as_ptr().cast()));
        root?
    };
    let (dir, exe) = protected_install_paths(&root)?;
    let dir = std::path::PathBuf::from(dir);
    if !no_reparse_ancestors(&dir) {
        return None;
    }
    Some((dir, std::path::PathBuf::from(exe)))
}

#[cfg(windows)]
fn no_reparse_ancestors(path: &std::path::Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    path.ancestors()
        .all(|part| match std::fs::symlink_metadata(part) {
            Ok(meta) => meta.file_attributes() & 0x400 == 0,
            Err(err) => err.kind() == std::io::ErrorKind::NotFound,
        })
}

#[cfg(windows)]
fn windows_user() -> Option<String> {
    let domain = std::env::var("USERDOMAIN").ok()?;
    let name = std::env::var("USERNAME").ok()?;
    if domain.is_empty()
        || name.is_empty()
        || domain.chars().any(char::is_control)
        || name.chars().any(char::is_control)
    {
        return None;
    }
    Some(format!(r"{domain}\{name}"))
}

#[cfg(windows)]
fn write_utf16(path: &std::path::Path, xml: &str) -> std::io::Result<()> {
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(path, bytes)
}

#[cfg(windows)]
fn wait_for_ready(process: Option<&std::os::windows::io::OwnedHandle>) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{GetProcessId, WaitForSingleObject};
    let handle = process.map(|p| HANDLE(p.as_raw_handle()));
    let expected_pid = handle.map(|h| unsafe { GetProcessId(h) });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if verified_ready(expected_pid).is_some() {
            return true;
        }
        if handle.is_some_and(|h| unsafe { WaitForSingleObject(h, 0) == WAIT_OBJECT_0 }) {
            return verified_ready(None).is_some();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    process.is_some() && verified_ready(None).is_some()
}

#[cfg(windows)]
fn schtasks(args: &[&str]) -> bool {
    system32("schtasks.exe", args)
}

#[cfg(windows)]
fn system32(tool: &str, args: &[&str]) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Some(system_dir) = system_directory() else {
        return false;
    };
    std::process::Command::new(system_dir.join(tool))
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(windows)]
enum LaunchError {
    Cancelled,
    Failed,
}

#[cfg(windows)]
fn shell_exec(
    file: &std::ffi::OsStr,
    params: &str,
    hidden: bool,
) -> Result<std::os::windows::io::OwnedHandle, LaunchError> {
    use std::os::windows::io::FromRawHandle;
    use windows::core::{w, HSTRING};
    use windows::Win32::Foundation::{ERROR_CANCELLED, WIN32_ERROR};
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL};
    let file = HSTRING::from(file);
    let params = HSTRING::from(params);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: if is_elevated() {
            w!("open")
        } else {
            w!("runas")
        },
        lpFile: windows::core::PCWSTR(file.as_ptr()),
        lpParameters: windows::core::PCWSTR(params.as_ptr()),
        nShow: if hidden { SW_HIDE.0 } else { SW_SHOWNORMAL.0 },
        ..Default::default()
    };
    unsafe {
        if let Err(err) = ShellExecuteExW(&mut info) {
            return Err(if WIN32_ERROR::from_error(&err) == Some(ERROR_CANCELLED) {
                LaunchError::Cancelled
            } else {
                LaunchError::Failed
            });
        }
        if info.hProcess.is_invalid() {
            return Err(LaunchError::Failed);
        }
        Ok(std::os::windows::io::OwnedHandle::from_raw_handle(
            info.hProcess.0,
        ))
    }
}

#[cfg(windows)]
fn shell_exec_wait(file: &std::ffi::OsStr, params: &str) -> Result<(), &'static str> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    let process = shell_exec(file, params, true).map_err(|err| match err {
        LaunchError::Cancelled => "已取消 UAC",
        LaunchError::Failed => "无法启动管理员安装程序",
    })?;
    let handle = HANDLE(process.as_raw_handle());
    let mut code = 1;
    unsafe {
        if WaitForSingleObject(handle, 30_000) != WAIT_OBJECT_0 {
            return Err("等待管理员安装程序超时或失败");
        }
        if GetExitCodeProcess(handle, &mut code).is_err() || code != 0 {
            return Err("管理员安装程序执行失败");
        }
    }
    Ok(())
}

#[cfg(windows)]
fn launch_protected_elevated() -> Result<std::os::windows::io::OwnedHandle, LaunchError> {
    let (_, exe) = protected_target().ok_or(LaunchError::Failed)?;
    if !exe.is_file() || !no_reparse_ancestors(&exe) {
        return Err(LaunchError::Failed);
    }
    shell_exec(exe.as_os_str(), RELAUNCH_FLAG, false)
}

#[cfg(windows)]
fn current_process_creation_time() -> Result<u64, String> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
        .map_err(|_| "无法验证当前进程身份".to_string())?;
    }
    Ok(((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
}

#[cfg(windows)]
fn process_creation_time(pid: u32) -> Result<u64, String> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|_| "无法验证清理进程身份".to_string())?;
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let result =
        unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) };
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(process);
    }
    result.map_err(|_| "无法验证清理进程身份".to_string())?;
    Ok(((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CleanupProcessObservation {
    Running,
    Exited(Option<u32>),
    Unknown,
}

#[cfg(windows)]
fn cleanup_process_observation(pid: u32, expected_created: u64) -> CleanupProcessObservation {
    use windows::Win32::Foundation::{
        ERROR_INVALID_PARAMETER, FILETIME, WAIT_OBJECT_0, WAIT_TIMEOUT, WIN32_ERROR,
    };
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, WaitForSingleObject,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };
    unsafe {
        let process = match OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        ) {
            Ok(process) => process,
            Err(error) if WIN32_ERROR::from_error(&error) == Some(ERROR_INVALID_PARAMETER) => {
                return CleanupProcessObservation::Exited(None)
            }
            Err(_) => return CleanupProcessObservation::Unknown,
        };
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user).is_err() {
            let _ = windows::Win32::Foundation::CloseHandle(process);
            return CleanupProcessObservation::Unknown;
        }
        let observed_created =
            (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        if observed_created != expected_created {
            let _ = windows::Win32::Foundation::CloseHandle(process);
            return CleanupProcessObservation::Exited(None);
        }
        match WaitForSingleObject(process, 0) {
            WAIT_TIMEOUT => {
                let _ = windows::Win32::Foundation::CloseHandle(process);
                CleanupProcessObservation::Running
            }
            WAIT_OBJECT_0 => {
                let mut code = 1u32;
                let result = GetExitCodeProcess(process, &mut code).is_ok();
                let _ = windows::Win32::Foundation::CloseHandle(process);
                if result {
                    CleanupProcessObservation::Exited(Some(code))
                } else {
                    CleanupProcessObservation::Unknown
                }
            }
            _ => {
                let _ = windows::Win32::Foundation::CloseHandle(process);
                CleanupProcessObservation::Unknown
            }
        }
    }
}

#[cfg(windows)]
fn wait_for_pid_created(
    pid: u32,
    expected_created: u64,
    timeout: std::time::Duration,
) -> Result<(), &'static str> {
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, WAIT_OBJECT_0, WAIT_TIMEOUT, WIN32_ERROR,
    };
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE,
    };
    unsafe {
        let handle = match OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        ) {
            Ok(handle) => handle,
            Err(err) if WIN32_ERROR::from_error(&err) == Some(ERROR_INVALID_PARAMETER) => {
                return Ok(())
            }
            Err(_) => return Err("无法确认旧进程已退出"),
        };
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user).is_err() {
            let _ = CloseHandle(handle);
            return Err("无法验证旧进程身份");
        }
        let observed = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
        if observed != expected_created {
            let _ = CloseHandle(handle);
            return Ok(());
        }
        let result =
            WaitForSingleObject(handle, u32::try_from(timeout.as_millis()).unwrap_or(10_000));
        let _ = CloseHandle(handle);
        match result {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => Err("等待旧进程退出超时"),
            _ => Err("等待旧进程失败"),
        }
    }
}

#[cfg(windows)]
fn wait_for_pid(pid: u32, timeout: std::time::Duration) -> Result<(), &'static str> {
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0, WAIT_TIMEOUT, WIN32_ERROR,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };
    unsafe {
        let handle = match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(handle) => handle,
            Err(err) if WIN32_ERROR::from_error(&err) == Some(ERROR_INVALID_PARAMETER) => {
                return Ok(())
            }
            Err(_) => return Err("无法确认旧进程已退出"),
        };
        let result =
            WaitForSingleObject(handle, u32::try_from(timeout.as_millis()).unwrap_or(10_000));
        let _ = CloseHandle(handle);
        match result {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => Err("等待旧进程退出超时"),
            _ => Err("等待旧进程失败"),
        }
    }
}

#[cfg(windows)]
fn wait_for_process_exit(
    process: &std::os::windows::io::OwnedHandle,
    timeout: std::time::Duration,
) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::WaitForSingleObject;
    let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    let result = unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), millis) };
    if result == WAIT_OBJECT_0 {
        Ok(())
    } else {
        Err("等待管理员关闭程序超时或失败".to_string())
    }
}

#[cfg(windows)]
fn wait_for_ordinary_process(
    child: &SuspendedChild,
    expected_exe: &std::path::Path,
    timeout: std::time::Duration,
) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::{GetCurrentProcessId, WaitForSingleObject};
    let mut current_session = 0u32;
    unsafe {
        ProcessIdToSessionId(GetCurrentProcessId(), &mut current_session)
            .map_err(|_| "无法确认桌面会话".to_string())?;
    }
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if unsafe { WaitForSingleObject(HANDLE(child.process.as_raw_handle()), 0) } == WAIT_OBJECT_0
        {
            return Err("普通权限程序启动后立即退出".to_string());
        }
        if let Some(hwnd) = visible_window_for_process(child.pid) {
            if let Some((record, elevated, image)) = process_facts(child.pid, hwnd) {
                if !elevated
                    && record.session == current_session
                    && same_file(&image, expected_exe)
                    && no_reparse_ancestors(expected_exe)
                    && window_belongs_to(hwnd, child.pid)
                    && owns_listener(child.pid)
                {
                    return Ok(());
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err("普通权限程序未在规定时间内完成启动验证".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(windows)]
fn visible_window_for_process(pid: u32) -> Option<isize> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible,
    };
    struct Search {
        pid: u32,
        hwnd: isize,
    }
    unsafe extern "system" fn visit(hwnd: HWND, parameter: LPARAM) -> BOOL {
        let search = &mut *(parameter.0 as *mut Search);
        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }
        let mut owner = 0u32;
        if GetWindowThreadProcessId(hwnd, Some(&mut owner)) != 0 && owner == search.pid {
            search.hwnd = hwnd.0 as isize;
            BOOL(0)
        } else {
            BOOL(1)
        }
    }
    let mut search = Search { pid, hwnd: 0 };
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM((&mut search as *mut Search) as isize));
    }
    (search.hwnd != 0).then_some(search.hwnd)
}

#[cfg(windows)]
struct SuspendedChild {
    process: std::os::windows::io::OwnedHandle,
    thread: std::os::windows::io::OwnedHandle,
    pid: u32,
    active: bool,
}

#[cfg(windows)]
impl SuspendedChild {
    fn resume(mut self) -> Result<Self, String> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Threading::ResumeThread;
        let previous = unsafe { ResumeThread(HANDLE(self.thread.as_raw_handle())) };
        if previous != 1 {
            return Err("普通权限程序无法恢复运行".to_string());
        }
        self.active = false;
        Ok(self)
    }

    fn terminate_and_wait(mut self) {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Threading::{TerminateProcess, WaitForSingleObject};
        unsafe {
            let _ = TerminateProcess(HANDLE(self.process.as_raw_handle()), 1);
            let _ = WaitForSingleObject(HANDLE(self.process.as_raw_handle()), 5_000);
        }
        self.active = false;
    }
}

#[cfg(windows)]
impl Drop for SuspendedChild {
    fn drop(&mut self) {
        if self.active {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::Foundation::HANDLE;
            use windows::Win32::System::Threading::TerminateProcess;
            unsafe {
                let process = HANDLE(self.process.as_raw_handle());
                let _ = TerminateProcess(process, 1);
                let _ = windows::Win32::System::Threading::WaitForSingleObject(process, 5_000);
            }
        }
    }
}

#[cfg(windows)]
fn create_suspended_medium_child(executable: &std::path::Path) -> Result<SuspendedChild, String> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::{
        DuplicateTokenEx, EqualSid, GetTokenInformation, TokenElevation, TokenPrimary,
        TokenSessionId, TokenUser, TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_SESSIONID,
        TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER,
    };
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::{
        CreateProcessWithTokenW, GetCurrentProcessId, OpenProcess, OpenProcessToken,
        QueryFullProcessImageNameW, CREATE_SUSPENDED, LOGON_WITH_PROFILE, PROCESS_INFORMATION,
        PROCESS_QUERY_LIMITED_INFORMATION, STARTF_USESHOWWINDOW, STARTUPINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetShellWindow, GetWindowThreadProcessId, SW_SHOWNORMAL,
    };

    fn info_buffer(
        token: HANDLE,
        class: windows::Win32::Security::TOKEN_INFORMATION_CLASS,
    ) -> Result<Vec<usize>, String> {
        use windows::Win32::Security::GetTokenInformation;
        let mut needed = 0u32;
        unsafe {
            let _ = GetTokenInformation(token, class, None, 0, &mut needed);
        }
        if needed == 0 || needed > 1024 * 1024 {
            return Err("无法验证普通用户身份".to_string());
        }
        let words = (needed as usize).div_ceil(std::mem::size_of::<usize>());
        let mut storage = vec![0usize; words];
        unsafe {
            GetTokenInformation(
                token,
                class,
                Some(storage.as_mut_ptr().cast()),
                needed,
                &mut needed,
            )
            .map_err(|_| "无法验证普通用户身份".to_string())?;
        }
        Ok(storage)
    }

    fn token_session(token: HANDLE) -> Result<u32, String> {
        let mut session = 0u32;
        let mut returned = 0u32;
        unsafe {
            GetTokenInformation(
                token,
                TokenSessionId,
                Some((&mut session as *mut u32).cast()),
                std::mem::size_of::<u32>() as u32,
                &mut returned,
            )
            .map_err(|_| "无法验证用户令牌会话".to_string())?;
        }
        Ok(session)
    }

    fn token_is_elevated(token: HANDLE) -> Result<bool, String> {
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut returned,
            )
            .map_err(|_| "无法验证普通用户权限".to_string())?;
        }
        Ok(elevation.TokenIsElevated != 0)
    }

    unsafe fn same_user(left: HANDLE, right: HANDLE) -> Result<bool, String> {
        let left_info = info_buffer(left, TokenUser)?;
        let right_info = info_buffer(right, TokenUser)?;
        let left_user = &*left_info.as_ptr().cast::<TOKEN_USER>();
        let right_user = &*right_info.as_ptr().cast::<TOKEN_USER>();
        Ok(EqualSid(left_user.User.Sid, right_user.User.Sid).is_ok())
    }

    let shell = unsafe { GetShellWindow() };
    if shell.0.is_null() {
        return Err("无法找到当前桌面的普通用户会话".to_string());
    }
    let mut shell_pid = 0u32;
    if unsafe { GetWindowThreadProcessId(shell, Some(&mut shell_pid)) } == 0 || shell_pid == 0 {
        return Err("无法验证当前桌面的普通用户会话".to_string());
    }
    let shell_process = unsafe {
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, shell_pid)
            .map_err(|_| "无法验证当前桌面的普通用户会话".to_string())?
    };
    let shell_process =
        unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(shell_process.0) };
    let mut shell_token = HANDLE::default();
    unsafe {
        OpenProcessToken(
            HANDLE(shell_process.as_raw_handle()),
            TOKEN_QUERY | TOKEN_DUPLICATE,
            &mut shell_token,
        )
        .map_err(|_| "无法读取普通用户会话令牌".to_string())?;
    }
    let shell_token = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(shell_token.0) };
    if token_is_elevated(HANDLE(shell_token.as_raw_handle()))? {
        return Err("当前桌面没有可用的普通权限令牌".to_string());
    }

    let current_pid = unsafe { GetCurrentProcessId() };
    let mut shell_session = 0u32;
    let mut current_session = 0u32;
    unsafe {
        ProcessIdToSessionId(shell_pid, &mut shell_session)
            .map_err(|_| "无法验证普通用户会话".to_string())?;
        ProcessIdToSessionId(current_pid, &mut current_session)
            .map_err(|_| "管理员实例与桌面不在同一会话".to_string())?;
    }
    let current_token = get_current_process_token()?;
    let shell_token_session = token_session(HANDLE(shell_token.as_raw_handle()))?;
    let current_token_session = token_session(HANDLE(current_token.as_raw_handle()))?;
    if shell_session != current_session
        || shell_token_session != current_token_session
        || shell_token_session != shell_session
        || !unsafe {
            same_user(
                HANDLE(shell_token.as_raw_handle()),
                HANDLE(current_token.as_raw_handle()),
            )?
        }
    {
        return Err("普通用户会话与管理员实例不匹配".to_string());
    }

    let mut primary = HANDLE::default();
    unsafe {
        DuplicateTokenEx(
            HANDLE(shell_token.as_raw_handle()),
            TOKEN_QUERY
                | TOKEN_DUPLICATE
                | TOKEN_ASSIGN_PRIMARY
                | TOKEN_ADJUST_DEFAULT
                | TOKEN_ADJUST_SESSIONID,
            None,
            windows::Win32::Security::SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        )
        .map_err(|_| "无法复制普通用户令牌".to_string())?;
    }
    let primary = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(primary.0) };
    if token_is_elevated(HANDLE(primary.as_raw_handle()))?
        || token_session(HANDLE(primary.as_raw_handle()))? != shell_token_session
        || !unsafe {
            same_user(
                HANDLE(shell_token.as_raw_handle()),
                HANDLE(primary.as_raw_handle()),
            )?
        }
    {
        return Err("复制的启动令牌不是当前用户的普通令牌".to_string());
    }

    let image: Vec<u16> = executable
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    if image.len() > 1024 || !executable.is_file() || !no_reparse_ancestors(executable) {
        return Err("普通程序路径无效或过长".to_string());
    }
    let mut command_line = quote_windows_wide(executable.as_os_str());
    if command_line.len() > 32767 {
        return Err("普通程序路径超过 Windows 启动限制".to_string());
    }
    let working = executable
        .parent()
        .ok_or_else(|| "普通程序目录无效".to_string())?;
    let working: Vec<u16> = working.as_os_str().encode_wide().chain(Some(0)).collect();
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        dwFlags: STARTF_USESHOWWINDOW,
        wShowWindow: SW_SHOWNORMAL.0 as u16,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    if let Err(error) = unsafe {
        CreateProcessWithTokenW(
            HANDLE(primary.as_raw_handle()),
            LOGON_WITH_PROFILE,
            PCWSTR(image.as_ptr()),
            Some(PWSTR(command_line.as_mut_ptr())),
            CREATE_SUSPENDED,
            None,
            PCWSTR(working.as_ptr()),
            &startup,
            &mut info,
        )
    } {
        let win32 = windows::Win32::Foundation::WIN32_ERROR::from_error(&error).map(|code| code.0);
        return Err(create_process_failure_message(
            win32,
            error.code().0 as u32,
            &error.message().to_string(),
        ));
    }
    let child = SuspendedChild {
        process: unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(info.hProcess.0) },
        thread: unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(info.hThread.0) },
        pid: info.dwProcessId,
        active: true,
    };
    let mut child_token = HANDLE::default();
    unsafe {
        OpenProcessToken(
            HANDLE(child.process.as_raw_handle()),
            TOKEN_QUERY,
            &mut child_token,
        )
        .map_err(|_| "无法验证普通程序权限".to_string())?;
    }
    let child_token = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(child_token.0) };
    let mut child_session = 0u32;
    unsafe {
        ProcessIdToSessionId(info.dwProcessId, &mut child_session)
            .map_err(|_| "无法验证普通程序会话".to_string())?;
    }
    let mut observed = vec![0u16; 32768];
    let mut observed_len = observed.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            HANDLE(child.process.as_raw_handle()),
            windows::Win32::System::Threading::PROCESS_NAME_WIN32,
            PWSTR(observed.as_mut_ptr()),
            &mut observed_len,
        )
        .map_err(|_| "无法验证普通程序路径".to_string())?;
    }
    let observed = std::path::PathBuf::from(std::ffi::OsString::from_wide(
        &observed[..observed_len as usize],
    ));
    if token_is_elevated(HANDLE(child_token.as_raw_handle()))?
        || token_session(HANDLE(child_token.as_raw_handle()))? != shell_token_session
        || child_session != shell_session
        || !unsafe {
            same_user(
                HANDLE(shell_token.as_raw_handle()),
                HANDLE(child_token.as_raw_handle()),
            )?
        }
        || !same_file(&observed, executable)
    {
        return Err("新程序未通过普通权限身份验证".to_string());
    }
    Ok(child)
}

#[cfg(any(windows, test))]
const STAGE_HELPER_NATIVE_EXIT_BASE: u32 = 0x4000_0000;
#[cfg(any(windows, test))]
const STAGE_HELPER_NATIVE_EXIT_MASK: u32 = 0xF000_0000;
#[cfg(any(windows, test))]
const STAGE_HELPER_GENERIC_EXIT: u32 = 2;
#[cfg(any(windows, test))]
const STAGE_HELPER_CLEANUP_BLOCKED_EXIT: u32 = 3;

#[cfg(any(windows, test))]
fn native_win32_code_from_reason(reason: &str) -> Option<u32> {
    let digits: String = reason
        .split_once("Win32 错误码 ")?
        .1
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let code = digits.parse::<u32>().ok()?;
    (code <= 0x0FFF_FFFF).then_some(code)
}

#[cfg(any(windows, test))]
fn stage_disable_helper_exit_code(reason: &str) -> i32 {
    if reason.contains(DISABLE_CLEANUP_BLOCKED_TAG) {
        return STAGE_HELPER_CLEANUP_BLOCKED_EXIT as i32;
    }
    native_win32_code_from_reason(reason).map_or(STAGE_HELPER_GENERIC_EXIT, |code| {
        STAGE_HELPER_NATIVE_EXIT_BASE | code
    }) as i32
}

#[cfg(any(windows, test))]
fn stage_disable_helper_failure_message(exit_code: u32) -> String {
    if exit_code == STAGE_HELPER_CLEANUP_BLOCKED_EXIT {
        "临时目录清理程序未能就绪或提前退出；管理员模式保持开启，本次交接未提交。请检查安全软件提示后，在设置中再次尝试关闭。".to_string()
    } else if exit_code & STAGE_HELPER_NATIVE_EXIT_MASK == STAGE_HELPER_NATIVE_EXIT_BASE {
        let code = exit_code & !STAGE_HELPER_NATIVE_EXIT_MASK;
        let hresult = 0x8007_0000 | (code & 0xFFFF);
        let message = std::io::Error::from_raw_os_error(code as i32);
        format!(
            "管理员关闭准备失败（Win32 错误码 {code} (0x{code:08X})，HRESULT_FROM_WIN32 0x{hresult:08X}）：{message}。管理员模式保持开启；处理提示中的系统原因后，可在设置中再次尝试关闭。"
        )
    } else {
        format!(
            "管理员关闭程序准备失败（辅助进程退出码 {exit_code}）；管理员模式保持开启，可在设置中再次尝试关闭。"
        )
    }
}

#[cfg(any(windows, test))]
fn create_process_failure_message(win32: Option<u32>, hresult: u32, message: &str) -> String {
    let code = win32.map_or_else(
        || format!("HRESULT 0x{hresult:08X}"),
        |code| format!("Win32 错误码 {code} (0x{code:08X})"),
    );
    format!(
        "CreateProcessWithTokenW 失败（{code}，HRESULT 0x{hresult:08X}）：{message}\n管理员模式保持开启；处理提示中的系统原因后，可在设置中再次尝试关闭。"
    )
}

#[cfg(windows)]
fn quote_windows_wide(arg: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    quote_windows_wide_units(&arg.encode_wide().collect::<Vec<_>>())
}

#[cfg(any(windows, test))]
fn quote_windows_wide_units(input: &[u16]) -> Vec<u16> {
    if !input.is_empty() && !input.iter().any(|unit| matches!(*unit, 0x20 | 0x09 | 0x22)) {
        return input.iter().copied().chain(Some(0)).collect();
    }
    let mut output = vec![0x22];
    let mut slashes = 0usize;
    for unit in input.iter().copied() {
        match unit {
            0x5c => slashes += 1,
            0x22 => {
                output.extend(std::iter::repeat_n(0x5c, slashes * 2 + 1));
                output.push(0x22);
                slashes = 0;
            }
            other => {
                output.extend(std::iter::repeat_n(0x5c, slashes));
                output.push(other);
                slashes = 0;
            }
        }
    }
    output.extend(std::iter::repeat_n(0x5c, slashes * 2));
    output.push(0x22);
    output.push(0);
    output
}

#[cfg(windows)]
fn get_current_process_token() -> Result<std::os::windows::io::OwnedHandle, String> {
    use std::os::windows::io::FromRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::TOKEN_QUERY;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token = HANDLE::default();
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|_| "无法读取管理员实例令牌".to_string())?;
        Ok(std::os::windows::io::OwnedHandle::from_raw_handle(token.0))
    }
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ReadyRecord {
    pid: u32,
    created: u64,
    session: u32,
    hwnd: isize,
}

#[cfg(any(windows, test))]
fn ready_matches(
    record: &ReadyRecord,
    observed: &ReadyRecord,
    expected_pid: Option<u32>,
    elevated: bool,
    listener: bool,
) -> bool {
    record == observed
        && record.pid != 0
        && record.created != 0
        && record.hwnd != 0
        && expected_pid.is_none_or(|pid| pid == record.pid)
        && elevated
        && listener
}

#[cfg(windows)]
fn process_facts(pid: u32, hwnd: isize) -> Option<(ReadyRecord, bool, std::path::PathBuf)> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, STILL_ACTIVE};
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, OpenProcessToken,
        QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let facts = (|| {
            let mut code = 0;
            GetExitCodeProcess(process, &mut code).ok()?;
            if code != STILL_ACTIVE.0 as u32 {
                return None;
            }
            let mut created = FILETIME::default();
            let mut exit = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            GetProcessTimes(process, &mut created, &mut exit, &mut kernel, &mut user).ok()?;
            let mut session = 0;
            ProcessIdToSessionId(pid, &mut session).ok()?;
            let mut path = [0u16; 32768];
            let mut len = path.len() as u32;
            QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                windows::core::PWSTR(path.as_mut_ptr()),
                &mut len,
            )
            .ok()?;
            let mut token = HANDLE::default();
            OpenProcessToken(process, TOKEN_QUERY, &mut token).ok()?;
            let mut elevation = TOKEN_ELEVATION::default();
            let mut returned = 0;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut returned,
            )
            .is_ok();
            let _ = CloseHandle(token);
            if !ok {
                return None;
            }
            Some((
                ReadyRecord {
                    pid,
                    created: (u64::from(created.dwHighDateTime) << 32)
                        | u64::from(created.dwLowDateTime),
                    session,
                    hwnd,
                },
                elevation.TokenIsElevated != 0,
                std::path::PathBuf::from(String::from_utf16(&path[..len as usize]).ok()?),
            ))
        })();
        let _ = CloseHandle(process);
        facts
    }
}

#[cfg(windows)]
fn owns_listener(pid: u32) -> bool {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    let mut size = 0;
    unsafe {
        let _ = GetExtendedTcpTable(None, &mut size, false, 2, TCP_TABLE_OWNER_PID_LISTENER, 0);
        if !(4..=16 * 1024 * 1024).contains(&size) {
            return false;
        }
        let mut bytes = vec![0u8; size as usize];
        if GetExtendedTcpTable(
            Some(bytes.as_mut_ptr().cast()),
            &mut size,
            false,
            2,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        ) != 0
        {
            return false;
        }
        let count = u32::from_ne_bytes(bytes[..4].try_into().unwrap()) as usize;
        let row_size = std::mem::size_of::<MIB_TCPROW_OWNER_PID>();
        if count > (bytes.len() - 4) / row_size {
            return false;
        }
        (0..count).any(|i| {
            let row = std::ptr::read_unaligned(
                bytes
                    .as_ptr()
                    .add(4 + i * row_size)
                    .cast::<MIB_TCPROW_OWNER_PID>(),
            );
            row.dwOwningPid == pid
                && row.dwLocalAddr == 0
                && u16::from_be(row.dwLocalPort as u16) == crate::protocol::PORT
        })
    }
}

#[cfg(windows)]
fn ready_path() -> Option<std::path::PathBuf> {
    let path = protected_state_dir()?.join("ready.json");
    no_reparse_ancestors(&path).then_some(path)
}

#[cfg(windows)]
pub fn publish_ready(hwnd: isize) -> std::io::Result<()> {
    let failure = || std::io::Error::other("cannot verify elevated readiness");
    if !is_elevated() || !owns_listener(std::process::id()) {
        return Err(failure());
    }
    let (record, elevated, exe) = process_facts(std::process::id(), hwnd).ok_or_else(failure)?;
    let (_, protected) = protected_target().ok_or_else(failure)?;
    if !elevated || !same_file(&exe, &protected) || !window_belongs_to(hwnd, record.pid) {
        return Err(failure());
    }
    let path = ready_path().ok_or_else(failure)?;
    std::fs::create_dir_all(path.parent().ok_or_else(failure)?)?;
    std::fs::write(&path, serde_json::to_vec(&record)?)?;
    let _ = std::fs::remove_file(path.with_file_name(STARTUP_ERROR_FILE));
    Ok(())
}

#[cfg(windows)]
fn window_belongs_to(raw: isize, pid: u32) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow};
    let hwnd = HWND(raw as *mut std::ffi::c_void);
    let mut owner = 0;
    unsafe {
        IsWindow(Some(hwnd)).as_bool()
            && GetWindowThreadProcessId(hwnd, Some(&mut owner)) != 0
            && owner == pid
    }
}

#[cfg(windows)]
fn verified_ready(expected_pid: Option<u32>) -> Option<ReadyRecord> {
    use std::io::Read;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::UI::WindowsAndMessaging::{
        IsIconic, IsWindowVisible, SetForegroundWindow, ShowWindowAsync, SW_RESTORE, SW_SHOW,
    };
    let mut bytes = Vec::new();
    std::fs::File::open(ready_path()?)
        .ok()?
        .take(1025)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 1024 {
        return None;
    }
    let record: ReadyRecord = serde_json::from_slice(&bytes).ok()?;
    let (observed, elevated, exe) = process_facts(record.pid, record.hwnd)?;
    let (_, protected) = protected_target()?;
    let mut session = 0;
    unsafe {
        ProcessIdToSessionId(std::process::id(), &mut session).ok()?;
    }
    if record.session != session
        || !same_file(&exe, &protected)
        || !no_reparse_ancestors(&exe)
        || !window_belongs_to(record.hwnd, record.pid)
        || !ready_matches(
            &record,
            &observed,
            expected_pid,
            elevated,
            owns_listener(record.pid),
        )
    {
        return None;
    }
    let hwnd = HWND(record.hwnd as *mut std::ffi::c_void);
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        }
        if !IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_SHOW);
        }
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        let _ = SetForegroundWindow(hwnd);
    }
    Some(record)
}

#[cfg(windows)]
fn recent_startup_error(attempt: std::time::SystemTime) -> Option<String> {
    use std::io::Read;
    let path = protected_state_dir()?.join(STARTUP_ERROR_FILE);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > STARTUP_ERROR_MAX as u64 {
        return None;
    }
    let modified = meta.modified().ok()?;
    let now = std::time::SystemTime::now();
    if !startup_note_is_current(modified, now, attempt) {
        return None;
    }
    let mut buf = Vec::new();
    std::fs::File::open(&path)
        .ok()?
        .take(STARTUP_ERROR_MAX as u64 + 1)
        .read_to_end(&mut buf)
        .ok()?;
    let age = now.duration_since(modified).ok()?;
    accept_startup_error(&buf, age)
}

/// Marker plus the protected copy. A user-writable preference cannot satisfy either.
#[cfg(any(windows, test))]
fn admin_mode_enabled(marker: bool, exe_exists: bool) -> bool {
    marker && exe_exists
}

#[cfg(any(windows, test))]
fn protected_install_paths(program_files: &str) -> Option<(String, String)> {
    let root = program_files.trim_end_matches(['\\', '/']);
    if root.is_empty() || root.chars().all(char::is_whitespace) {
        return None;
    }
    let dir = format!(r"{root}\AgentsPads");
    let exe = format!(r"{dir}\agentspads.exe");
    Some((dir, exe))
}

#[cfg(any(windows, test))]
enum EarlyAction {
    None,
    Install(String, bool),
    Remove,
    StageDisable(u32, u64),
    DisableWorker(u32, u64),
    Bad,
}

#[cfg(any(windows, test))]
fn early_action<I, S>(args: I) -> EarlyAction
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args.into_iter().map(|a| a.as_ref().to_string()).collect();
    let mut args = args.iter().map(String::as_str);
    while let Some(arg) = args.next() {
        match arg {
            REMOVE_FLAG => return EarlyAction::Remove,
            STAGE_DISABLE_FLAG => {
                return parse_internal_pid(&mut args)
                    .map(|(pid, created)| EarlyAction::StageDisable(pid, created))
                    .unwrap_or(EarlyAction::Bad)
            }
            DISABLE_WORKER_FLAG => {
                return parse_internal_pid(&mut args)
                    .map(|(pid, created)| EarlyAction::DisableWorker(pid, created))
                    .unwrap_or(EarlyAction::Bad)
            }
            INSTALL_FLAG => {
                let Some(user) = args.next() else {
                    return EarlyAction::Bad;
                };
                if user.is_empty() || user.starts_with('-') || user.chars().any(char::is_control) {
                    return EarlyAction::Bad;
                }
                let autostart = args.any(|a| a == AUTOSTART_FLAG);
                return EarlyAction::Install(user.to_string(), autostart);
            }
            _ => {}
        }
    }
    EarlyAction::None
}

#[cfg(any(windows, test))]
fn parse_internal_pid<'a>(args: &mut impl Iterator<Item = &'a str>) -> Option<(u32, u64)> {
    let pid = args.next()?.parse::<u32>().ok()?;
    let created = args.next()?.parse::<u64>().ok()?;
    (pid != 0 && created != 0 && args.next().is_none()).then_some((pid, created))
}

/// `CommandLineToArgvW` quoting. Backslashes are only special before a quote.
#[cfg(any(windows, test))]
fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|c| matches!(c, ' ' | '\t' | '"')) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            other => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(other);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

#[cfg(any(windows, test))]
fn join_windows_args(args: &[&str]) -> String {
    args.iter()
        .copied()
        .map(quote_windows_arg)
        .collect::<Vec<_>>()
        .join(" ")
}

/// `logon: None` is the on-demand task (no trigger, used by manual launches).
/// `Some(enabled)` is the login-autostart task: same action, plus a delayed
/// logon trigger, and the whole task is switched with `schtasks /Change
/// /ENABLE|/DISABLE` (a disabled task cannot be run on demand, so it must be a
/// separate task). Priority 4 keeps the process at normal priority; the task
/// default (7) would lag input injection.
#[cfg(any(windows, test))]
fn task_xml(exe: &str, user: Option<&str>, logon: Option<bool>) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let user = user
        .map(|u| format!("<UserId>{}</UserId>", esc(u)))
        .unwrap_or_default();
    let triggers = if logon.is_some() {
        format!(
            "<Triggers><LogonTrigger><Enabled>true</Enabled>{user}<Delay>{LOGON_DELAY}</Delay></LogonTrigger></Triggers>"
        )
    } else {
        String::new()
    };
    let enabled = if logon == Some(false) {
        "\n    <Enabled>false</Enabled>"
    } else {
        ""
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>{TASK_NAME}</Description></RegistrationInfo>
  {triggers}
  <Principals>
    <Principal id="Author">{user}<LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>{enabled}
  </Settings>
  <Actions Context="Author">
    <Exec><Command>{}</Command><Arguments>{RELAUNCH_FLAG}</Arguments></Exec>
  </Actions>
</Task>
"#,
        esc(exe)
    )
}

#[cfg(any(windows, test))]
fn handoff_parent_pid<I, S>(args: I) -> Option<u32>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args.into_iter().map(|a| a.as_ref().to_string()).collect();
    args.windows(2).find_map(|pair| {
        if pair[0] != HANDOFF_FLAG {
            return None;
        }
        let pid = pair[1].parse::<u32>().ok()?;
        (pid != 0).then_some(pid)
    })
}

#[cfg(any(windows, test))]
fn failed_handoff_outcome(startup_error: Option<&str>) -> RelaunchOutcome {
    if startup_error.is_some_and(is_identity_store_failure_note) {
        RelaunchOutcome::ProtectedIdentityStoreFailed
    } else {
        RelaunchOutcome::ContinueOrdinaryStartup
    }
}

#[cfg(any(windows, test))]
fn is_identity_store_failure_note(line: &str) -> bool {
    let Some((message, timestamp)) = line.rsplit_once(' ') else {
        return false;
    };
    if !is_utc_timestamp(timestamp) {
        return false;
    }
    if message == "identity store failed" {
        return true;
    }
    let Some(detail) = message.strip_prefix("identity store failed: ") else {
        return false;
    };
    !detail.is_empty()
        && detail == detail.trim()
        && detail.chars().count() <= 80
        && !detail
            .chars()
            .any(|character| character == '\\' || character == '/' || character.is_control())
}

#[cfg(any(windows, test))]
fn is_utc_timestamp(timestamp: &str) -> bool {
    let bytes = timestamp.as_bytes();
    if bytes.len() != 20 {
        return false;
    }
    let fixed = [
        (4, b'-'),
        (7, b'-'),
        (10, b'T'),
        (13, b':'),
        (16, b':'),
        (19, b'Z'),
    ];
    if fixed
        .iter()
        .any(|(index, expected)| bytes[*index] != *expected)
    {
        return false;
    }
    if bytes.iter().enumerate().any(|(index, byte)| {
        !fixed.iter().any(|(fixed_index, _)| *fixed_index == index) && !byte.is_ascii_digit()
    }) {
        return false;
    }
    let month = timestamp[5..7].parse::<u32>().ok();
    let day = timestamp[8..10].parse::<u32>().ok();
    let hour = timestamp[11..13].parse::<u32>().ok();
    let minute = timestamp[14..16].parse::<u32>().ok();
    let second = timestamp[17..19].parse::<u32>().ok();
    matches!(month, Some(1..=12))
        && matches!(day, Some(1..=31))
        && matches!(hour, Some(0..=23))
        && matches!(minute, Some(0..=59))
        && matches!(second, Some(0..=59))
}

#[cfg(any(windows, test))]
fn explain_handoff_failure(
    task_started: bool,
    cancelled: bool,
    startup_error: Option<&str>,
) -> String {
    if cancelled {
        return "已取消 UAC".to_string();
    }
    if let Some(detail) = startup_error.map(str::trim).filter(|text| !text.is_empty()) {
        return format!("管理员实例未就绪：{detail}");
    }
    if task_started {
        "等待管理员实例就绪超时".to_string()
    } else {
        "计划任务及 UAC 回退未能启动管理员实例".to_string()
    }
}

/// Drop path-like or huge details. The note is readable by ordinary users.
#[cfg(any(windows, test))]
fn startup_failure_text(kind: &str, detail: &str) -> String {
    let detail = detail.split(['\r', '\n']).next().unwrap_or("").trim();
    let usable = !detail.is_empty()
        && detail != kind
        && detail.chars().count() <= 80
        && !detail
            .chars()
            .any(|c| c == '\\' || c == '/' || c.is_control());
    if usable {
        format!("{kind}: {detail}")
    } else {
        kind.to_string()
    }
}

#[cfg(any(windows, test))]
fn startup_error_line(reason: &str, unix_secs: u64) -> String {
    let reason = reason.split(['\r', '\n']).next().unwrap_or("").trim();
    let reason = if reason.is_empty() {
        "startup failed"
    } else {
        reason
    };
    let stamp = unix_utc_stamp(unix_secs);
    format!("{reason} {stamp}\n")
}

#[cfg(any(windows, test))]
fn accept_startup_error(bytes: &[u8], age: std::time::Duration) -> Option<String> {
    if bytes.is_empty() || bytes.len() > STARTUP_ERROR_MAX || age > STARTUP_ERROR_MAX_AGE {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let line = text.lines().next()?.trim();
    if line.is_empty() || line.chars().any(char::is_control) {
        return None;
    }
    Some(line.to_string())
}

#[cfg(any(windows, test))]
fn startup_note_is_current(
    modified: std::time::SystemTime,
    now: std::time::SystemTime,
    attempt: std::time::SystemTime,
) -> bool {
    if modified < attempt {
        return false;
    }
    now.duration_since(modified)
        .is_ok_and(|age| age <= STARTUP_ERROR_MAX_AGE)
}

#[cfg(any(windows, test))]
fn unix_utc_stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    let h = tod / 3_600;
    let min = (tod % 3_600) / 60;
    let s = tod % 60;
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}Z")
}

/// Days since 1970-01-01 to a civil date. Howard Hinnant, public domain.
#[cfg(any(windows, test))]
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disable_stage_status_is_bound_to_both_process_identities() {
        let ready = encode_disable_stage_status(&DisableStageState::Ready, 42, 1234, 77);
        let record = parse_disable_stage_status(ready.as_bytes()).unwrap();
        assert_eq!(validate_disable_stage_record(record, 42, 1234, 77), Ok(()));
        for wrong in [(43, 1234, 77), (42, 1235, 77), (42, 1234, 78)] {
            let record = parse_disable_stage_status(ready.as_bytes()).unwrap();
            assert!(validate_disable_stage_record(record, wrong.0, wrong.1, wrong.2).is_err());
        }

        {
            let state = DisableStageState::Failed("native error".to_string());
            let encoded = encode_disable_stage_status(&state, 42, 1234, 77);
            let record = parse_disable_stage_status(encoded.as_bytes()).unwrap();
            assert_eq!(
                validate_disable_stage_record(record, 42, 1234, 77),
                Err(match state {
                    DisableStageState::Failed(reason) => reason,
                    DisableStageState::Ready => unreachable!(),
                })
            );
        }
    }

    #[test]
    fn disable_stage_cancel_first_or_after_ready_never_authorizes_unload() {
        let ready = encode_disable_stage_status(&DisableStageState::Ready, 42, 1234, 77);
        let ready_record = parse_disable_stage_status(ready.as_bytes()).unwrap();
        let ready_result = validate_disable_stage_record(ready_record, 42, 1234, 77);
        let cancel = encode_disable_stage_decision(&DisableStageDecision::Cancel, 42, 1234, 77);
        let cancel_record = parse_disable_stage_decision(cancel.as_bytes()).unwrap();
        assert_eq!(
            validate_disable_stage_decision(
                cancel_record,
                42,
                1234,
                77,
                DisableStageDecision::Cancel,
            ),
            Ok(())
        );

        assert!(authorize_disable_stage_unload(
            Err("READY not yet published".to_string()),
            None,
            Some(DisableStageDecision::Cancel),
        )
        .is_err());
        assert!(authorize_disable_stage_unload(
            ready_result.clone(),
            None,
            Some(DisableStageDecision::Cancel),
        )
        .is_err());
        assert!(authorize_disable_stage_unload(
            ready_result.clone(),
            Some(DisableStageDecision::Acknowledge),
            Some(DisableStageDecision::Cancel),
        )
        .is_err());
        assert_eq!(
            authorize_disable_stage_unload(
                ready_result,
                Some(DisableStageDecision::Acknowledge),
                None,
            ),
            Ok(())
        );
    }

    #[test]
    fn disable_stage_commit_requires_acknowledged_worker_and_matching_final_choice() {
        let acked =
            encode_disable_stage_decision(&DisableStageDecision::Acknowledged, 42, 1234, 77);
        let acked_record = parse_disable_stage_decision(acked.as_bytes()).unwrap();
        assert_eq!(
            validate_disable_stage_decision(
                acked_record,
                42,
                1234,
                77,
                DisableStageDecision::Acknowledged,
            ),
            Ok(())
        );
        for wrong in [(43, 1234, 77), (42, 1235, 77), (42, 1234, 78)] {
            let record = parse_disable_stage_decision(acked.as_bytes()).unwrap();
            assert!(validate_disable_stage_decision(
                record,
                wrong.0,
                wrong.1,
                wrong.2,
                DisableStageDecision::Acknowledged,
            )
            .is_err());
        }
        assert!(authorize_disable_stage_commit(None, Some(DisableStageDecision::Commit)).is_err());
        assert!(authorize_disable_stage_commit(
            Some(DisableStageDecision::Acknowledged),
            Some(DisableStageDecision::Abort),
        )
        .is_err());
        assert!(
            authorize_disable_stage_commit(Some(DisableStageDecision::Acknowledged), None,)
                .is_err()
        );
        assert_eq!(
            authorize_disable_stage_commit(
                Some(DisableStageDecision::Acknowledged),
                Some(DisableStageDecision::Commit),
            ),
            Ok(())
        );
    }

    #[test]
    fn disable_stage_ack_and_cancel_are_bound_to_parent_creation_and_worker() {
        for decision in [
            DisableStageDecision::Acknowledge,
            DisableStageDecision::Cancel,
        ] {
            let encoded = encode_disable_stage_decision(&decision, 42, 1234, 77);
            let record = parse_disable_stage_decision(encoded.as_bytes()).unwrap();
            assert_eq!(
                validate_disable_stage_decision(record, 42, 1234, 77, decision,),
                Ok(())
            );
            for wrong in [(43, 1234, 77), (42, 1235, 77), (42, 1234, 78)] {
                let record = parse_disable_stage_decision(encoded.as_bytes()).unwrap();
                assert!(validate_disable_stage_decision(
                    record, wrong.0, wrong.1, wrong.2, decision,
                )
                .is_err());
            }
        }
    }

    #[test]
    fn stage_helper_exit_returns_native_error_to_the_original_ui_without_api_misattribution() {
        let reason = create_process_failure_message(Some(5), 0x8007_0005, "Access is denied.");
        let exit_code = stage_disable_helper_exit_code(&reason) as u32;
        assert_eq!(exit_code, STAGE_HELPER_NATIVE_EXIT_BASE | 5);
        let returned = stage_disable_helper_failure_message(exit_code);
        assert!(returned.contains("Win32 错误码 5 (0x00000005)"));
        assert!(returned.contains("HRESULT_FROM_WIN32 0x80070005"));
        assert!(!returned.contains("HRESULT_FROM_WIN32 0x80070005）：。管理员模式"));
        assert!(returned.contains("管理员模式保持开启"));
        assert!(!returned.contains("CreateProcessWithTokenW 失败"));
        let mutex_reason = "无法创建关闭流程互斥锁（Win32 错误码 5）：Access is denied.";
        assert_eq!(
            stage_disable_helper_exit_code(mutex_reason),
            exit_code as i32
        );
        assert!(
            !stage_disable_helper_failure_message(exit_code).contains("CreateProcessWithTokenW")
        );
        assert_eq!(stage_disable_helper_exit_code("some internal failure"), 2);
        assert!(stage_disable_helper_failure_message(2).contains("辅助进程退出码 2"));
        let blocked = stage_disable_helper_exit_code(&cleanup_blocked_reason("blocked")) as u32;
        assert_eq!(blocked, STAGE_HELPER_CLEANUP_BLOCKED_EXIT);
        let message = stage_disable_helper_failure_message(blocked);
        assert!(message.contains("管理员模式保持开启"));
        assert!(message.contains("检查安全软件提示"));
        assert!(message.contains("再次尝试关闭"));
    }

    #[test]
    fn create_process_failure_keeps_native_code_and_message() {
        let reason = create_process_failure_message(Some(5), 0x8007_0005, "Access is denied.");
        assert!(reason.contains("Win32 错误码 5 (0x00000005)"));
        assert!(reason.contains("HRESULT 0x80070005"));
        assert!(reason.contains("Access is denied."));
        assert!(reason.contains("管理员模式保持开启"));
        assert!(reason.contains("设置中再次尝试关闭"));
        assert!(!reason.contains("火绒"));
        assert!(!reason.contains("杀毒软件"));
    }

    #[test]
    fn worker_cleanup_wait_has_no_fixed_timeout_and_binds_creation_time() {
        let script = worker_cleanup_script(77, 123456, "encoded-path");
        assert!(script.contains("$p.WaitForExit()"));
        assert!(!script.contains("Wait-Process"));
        assert!(!script.contains("-Timeout"));
        assert!(script.contains("$p.StartTime.ToUniversalTime().ToFileTimeUtc()"));
        assert!(script.contains("cleanup-ready.txt"));
        assert!(script.contains("exit 0"));
        assert!(script.contains("exit 1"));
        assert!(script.contains("Get-Process -Id 77 -ErrorAction Stop"));
        assert!(
            script.find("if ($created -ne 123456)").unwrap() < script.find("WriteAllText").unwrap()
        );
    }

    #[test]
    fn cleanup_executor_must_be_ready_and_live_before_commit() {
        assert!(!cleanup_executor_can_commit(false, true));
        assert!(!cleanup_executor_can_commit(true, false));
        assert!(cleanup_executor_can_commit(true, true));
    }

    #[test]
    fn cleanup_registration_binds_both_process_creation_times() {
        let registration = DisableCleanupRegistration {
            worker_pid: 45064,
            worker_created: 134360853955530016,
            cleanup_pid: 35888,
            cleanup_created: 134360853955530017,
        };
        let pending = encode_disable_cleanup_registration(registration, "PENDING");
        assert_eq!(
            parse_disable_cleanup_registration(&pending, "PENDING").unwrap(),
            registration
        );
        let ready = encode_disable_cleanup_registration(registration, "READY");
        assert_eq!(
            parse_disable_cleanup_registration(&ready, "READY").unwrap(),
            registration
        );
        assert!(parse_disable_cleanup_registration(&ready, "PENDING").is_err());
        assert!(parse_disable_cleanup_registration(
            "READY 45064 134360853955530016 35888 134360853955530017 extra",
            "READY"
        )
        .is_err());
        assert!(parse_disable_cleanup_registration("READY 0 1 2 3", "READY").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn terminal_stage_state_move_never_replaces_the_first_result() {
        use std::io::Write;
        let dir =
            std::env::temp_dir().join(format!("AgentsPads-stage-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let _cleanup = TempTree(dir.clone());
        for (first, second) in [
            (b"CANCEL".as_slice(), b"READY".as_slice()),
            (b"READY", b"CANCEL"),
        ] {
            let destination = dir.join(format!("status-{}", first[0]));
            let first_temp = dir.join(format!("first-{}", first[0]));
            let second_temp = dir.join(format!("second-{}", first[0]));
            std::fs::File::create(&first_temp)
                .unwrap()
                .write_all(first)
                .unwrap();
            std::fs::File::create(&second_temp)
                .unwrap()
                .write_all(second)
                .unwrap();
            assert!(move_file_without_replacing(&first_temp, &destination));
            assert!(!move_file_without_replacing(&second_temp, &destination));
            assert_eq!(std::fs::read(&destination).unwrap(), first);
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires an elevated Windows token and AGENTPAD_TEST_MEDIUM_EXE"]
    fn elevated_medium_process_creation_probe_only_terminates_its_child() {
        let path = std::env::var_os("AGENTPAD_TEST_MEDIUM_EXE")
            .expect("set AGENTPAD_TEST_MEDIUM_EXE to the ordinary source executable");
        let child = create_suspended_medium_child(std::path::Path::new(&path))
            .unwrap_or_else(|reason| panic!("CreateProcessWithTokenW probe failed: {reason}"));
        assert_ne!(child.pid, 0);
        child.terminate_and_wait();
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires an elevated Windows token"]
    fn staged_worker_file_inherits_the_verified_admin_system_acl() {
        let dir =
            std::env::temp_dir().join(format!("AgentsPads-worker-acl-{}", uuid::Uuid::new_v4()));
        create_private_worker_directory(&dir).unwrap();
        let _cleanup = TempTree(dir.clone());
        let worker = dir.join("agentspads-disable-worker.exe");
        std::fs::write(&worker, b"fixture").unwrap();
        set_administrators_owner(&worker).unwrap();
        validate_admin_system_acl(&dir, true).unwrap();
        validate_admin_system_acl(&worker, false).unwrap();
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires an elevated Windows token"]
    fn an_existing_empty_dacl_identity_file_is_repaired_before_opening() {
        use std::io::Read;
        use std::os::windows::ffi::OsStrExt;

        #[repr(C)]
        struct AclHeader {
            revision: u8,
            sbz1: u8,
            size: u16,
            ace_count: u16,
            sbz2: u16,
        }

        #[repr(C)]
        struct AceHeader {
            ace_type: u8,
            flags: u8,
            size: u16,
        }

        #[link(name = "advapi32")]
        extern "system" {
            fn SetNamedSecurityInfoW(
                name: *mut u16,
                object_type: u32,
                security_information: u32,
                owner: *mut std::ffi::c_void,
                group: *mut std::ffi::c_void,
                dacl: *mut AclHeader,
                sacl: *mut std::ffi::c_void,
            ) -> u32;
            fn GetNamedSecurityInfoW(
                name: *mut u16,
                object_type: u32,
                security_information: u32,
                owner: *mut *mut std::ffi::c_void,
                group: *mut *mut std::ffi::c_void,
                dacl: *mut *mut AclHeader,
                sacl: *mut *mut std::ffi::c_void,
                descriptor: *mut *mut std::ffi::c_void,
            ) -> u32;
            fn ConvertSidToStringSidW(sid: *mut std::ffi::c_void, string_sid: *mut *mut u16)
                -> i32;
            fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        }

        struct TempAclTree(std::path::PathBuf);
        impl Drop for TempAclTree {
            fn drop(&mut self) {
                let path = self.0.to_string_lossy().into_owned();
                let _ = system32("icacls.exe", &[&path, "/reset", "/T"]);
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        unsafe fn sid_text(sid: *mut std::ffi::c_void) -> String {
            let mut text = std::ptr::null_mut();
            assert_ne!(ConvertSidToStringSidW(sid, &mut text), 0);
            let mut len = 0;
            while *text.add(len) != 0 {
                len += 1;
            }
            let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            let _ = LocalFree(text.cast());
            value
        }

        unsafe fn acl_snapshot(path: &std::path::Path) -> (String, Vec<(String, u32, u8, u8)>) {
            let mut name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let mut owner = std::ptr::null_mut();
            let mut dacl = std::ptr::null_mut();
            let mut descriptor = std::ptr::null_mut();
            assert_eq!(
                GetNamedSecurityInfoW(
                    name.as_mut_ptr(),
                    1,
                    0x1 | 0x4,
                    &mut owner,
                    std::ptr::null_mut(),
                    &mut dacl,
                    std::ptr::null_mut(),
                    &mut descriptor,
                ),
                0
            );
            let owner_text = sid_text(owner);
            let mut rules = Vec::new();
            let acl = &*dacl;
            let mut ace = (dacl.cast::<u8>()).add(std::mem::size_of::<AclHeader>());
            for _ in 0..acl.ace_count {
                let header = &*(ace.cast::<AceHeader>());
                let mask = if header.ace_type == 0 {
                    std::ptr::read_unaligned(
                        ace.add(std::mem::size_of::<AceHeader>()).cast::<u32>(),
                    )
                } else {
                    0
                };
                let sid = ace.add(std::mem::size_of::<AceHeader>() + std::mem::size_of::<u32>());
                rules.push((sid_text(sid.cast()), mask, header.ace_type, header.flags));
                ace = ace.add(header.size as usize);
            }
            let _ = LocalFree(descriptor);
            (owner_text, rules)
        }

        unsafe fn set_empty_dacl(path: &std::path::Path) {
            let mut name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let mut empty = AclHeader {
                revision: 2,
                sbz1: 0,
                size: std::mem::size_of::<AclHeader>() as u16,
                ace_count: 0,
                sbz2: 0,
            };
            assert_eq!(
                SetNamedSecurityInfoW(
                    name.as_mut_ptr(),
                    1,
                    0x4 | 0x8000_0000,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut empty,
                    std::ptr::null_mut(),
                ),
                0
            );
        }

        let dir = std::env::temp_dir().join(format!("AgentsPads-acl-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let _cleanup = TempAclTree(dir.clone());
        let file = dir.join("identity.json");
        let fixture = b"identity test fixture; never a real secret";
        std::fs::write(&file, fixture).unwrap();
        let mut existing_handle = std::fs::File::open(&file).unwrap();
        unsafe { set_empty_dacl(&file) };
        let (empty_owner, empty_rules) = unsafe { acl_snapshot(&file) };
        assert!(
            empty_rules.is_empty(),
            "fixture must have a real empty Windows DACL"
        );

        let prepared = prepare_admin_identity_at(&dir);
        assert!(prepared.is_ok(), "ACL recovery failed: {prepared:?}");
        let file = prepared.unwrap();
        let (dir_owner, dir_rules) = unsafe { acl_snapshot(&dir) };
        let (file_owner, file_rules) = unsafe { acl_snapshot(&file) };
        assert_eq!(dir_owner, "S-1-5-32-544");
        assert_eq!(file_owner, "S-1-5-32-544");
        assert_eq!(empty_owner, file_owner);
        assert_eq!(dir_rules.len(), 2);
        assert_eq!(file_rules.len(), 2);
        for (sid, mask, ace_type, flags) in dir_rules {
            assert!(sid == "S-1-5-32-544" || sid == "S-1-5-18");
            assert_eq!(ace_type, 0);
            assert_eq!(mask & 0x001F_01FF, 0x001F_01FF);
            assert_eq!(flags & 0x03, 0x03);
        }
        for (sid, mask, ace_type, flags) in file_rules {
            assert!(sid == "S-1-5-32-544" || sid == "S-1-5-18");
            assert_eq!(ace_type, 0);
            assert_eq!(mask & 0x001F_01FF, 0x001F_01FF);
            assert_eq!(flags & 0x13, 0);
        }
        let mut actual = Vec::new();
        existing_handle.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, fixture);
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&file)
            .unwrap();
    }
    #[test]
    fn elevation_attempt_cannot_accept_an_ordinary_token() {
        assert!(reject_ordinary_attempt(true, false));
        assert!(!reject_ordinary_attempt(true, true));
        assert!(!reject_ordinary_attempt(false, false));
    }

    #[test]
    fn ready_requires_the_same_live_elevated_listener_and_window() {
        let ready = ReadyRecord {
            pid: 42,
            created: 100,
            session: 1,
            hwnd: 88,
        };
        assert!(ready_matches(&ready, &ready, Some(42), true, true));
        assert!(!ready_matches(&ready, &ready, Some(43), true, true));
        assert!(!ready_matches(&ready, &ready, None, false, true));
        assert!(!ready_matches(&ready, &ready, None, true, false));
        for observed in [
            ReadyRecord { pid: 43, ..ready },
            ReadyRecord {
                created: 101,
                ..ready
            },
            ReadyRecord {
                session: 2,
                ..ready
            },
            ReadyRecord { hwnd: 0, ..ready },
        ] {
            assert!(!ready_matches(&ready, &observed, None, true, true));
        }
    }

    #[test]
    fn task_runs_elevated_on_demand_at_normal_priority() {
        let (_dir, exe) = protected_install_paths(r"C:\Program Files").unwrap();
        let xml = task_xml(&exe, Some(r"PC\jo"), None);
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains(r"<UserId>PC\jo</UserId>"));
        assert!(xml.contains(&format!("<Command>{exe}</Command>")));
        assert!(xml.contains(r"<Command>C:\Program Files\AgentsPads\agentspads.exe</Command>"));
        assert!(xml.contains("<Arguments>--elevated</Arguments>"));
        assert!(xml.contains("<Priority>4</Priority>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(!xml.contains("Trigger"));
        assert!(!task_xml("a.exe", None, None).contains("UserId"));
        let escaped = task_xml(r"C:\Apps\A&B <x>\agentpad.exe", Some(r"PC\jo"), None);
        assert!(escaped.contains(r"<Command>C:\Apps\A&amp;B &lt;x&gt;\agentpad.exe</Command>"));
    }

    #[test]
    fn autostart_task_has_delayed_logon_trigger_and_switchable_state() {
        let on = task_xml("a.exe", Some(r"PC\jo"), Some(true));
        assert!(on.contains("<LogonTrigger>"));
        assert!(on.contains(r"<UserId>PC\jo</UserId><Delay>PT30S</Delay>"));
        assert!(on.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(on.contains("<Arguments>--elevated</Arguments>"));
        assert!(!on.contains("<Enabled>false</Enabled>"));
        let off = task_xml("a.exe", Some(r"PC\jo"), Some(false));
        assert!(off.contains("<LogonTrigger>"));
        assert!(off.contains("<Enabled>false</Enabled>"));
        assert_ne!(AUTOSTART_TASK_NAME, TASK_NAME);
    }

    #[test]
    fn protected_install_paths_reject_empty_roots() {
        assert_eq!(
            protected_install_paths(r"C:\Program Files\"),
            Some((
                r"C:\Program Files\AgentsPads".to_string(),
                r"C:\Program Files\AgentsPads\agentspads.exe".to_string(),
            ))
        );
        assert_eq!(
            protected_install_paths(r"D:\PF\\"),
            Some((
                r"D:\PF\AgentsPads".to_string(),
                r"D:\PF\AgentsPads\agentspads.exe".to_string(),
            ))
        );
        assert_eq!(protected_install_paths(""), None);
        assert_eq!(protected_install_paths(r"\\"), None);
        assert_eq!(protected_install_paths("   "), None);
    }

    #[test]
    fn admin_mode_requires_both_marker_and_protected_copy() {
        assert!(admin_mode_enabled(true, true));
        assert!(!admin_mode_enabled(true, false));
        assert!(!admin_mode_enabled(false, true));
    }

    #[test]
    fn disable_worker_path_must_be_a_direct_program_data_child() {
        let fixture =
            std::env::temp_dir().join(format!("AgentsPads-boundary-{}", uuid::Uuid::new_v4()));
        let program_data = fixture.join("ProgramData");
        std::fs::create_dir_all(&program_data).unwrap();
        let _cleanup = TempTree(fixture.clone());
        let worker_name = format!("AgentsPads-disable-{}", uuid::Uuid::new_v4());
        let worker_dir = program_data.join(&worker_name);
        std::fs::create_dir(&worker_dir).unwrap();
        let worker_exe = worker_dir.join("agentspads-disable-worker.exe");
        std::fs::write(&worker_exe, b"fixture").unwrap();
        assert!(valid_disable_worker_boundary(
            &program_data,
            &worker_dir,
            &worker_exe
        ));

        let nested_parent = program_data.join("nested");
        std::fs::create_dir(&nested_parent).unwrap();
        let nested_dir = nested_parent.join(format!("AgentsPads-disable-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&nested_dir).unwrap();
        let nested_exe = nested_dir.join("agentspads-disable-worker.exe");
        std::fs::write(&nested_exe, b"fixture").unwrap();
        assert!(!valid_disable_worker_boundary(
            &program_data,
            &nested_dir,
            &nested_exe
        ));

        let program_data_exe = program_data.join("agentspads-disable-worker.exe");
        std::fs::write(&program_data_exe, b"fixture").unwrap();
        assert!(!valid_disable_worker_boundary(
            &program_data,
            &program_data,
            &program_data_exe
        ));

        let wrong_name = worker_dir.join("other.exe");
        std::fs::write(&wrong_name, b"fixture").unwrap();
        assert!(!valid_disable_worker_boundary(
            &program_data,
            &worker_dir,
            &wrong_name
        ));
    }

    struct TempTree(std::path::PathBuf);

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn wide_command_line_is_always_nul_terminated() {
        let no_space: Vec<u16> = "C:\\Apps\\agentpad.exe"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let empty = [0x22, 0x22, 0];
        let spaced: Vec<u16> = "\"C:\\Program Files\\AgentsPads\\agentpad.exe\"\0"
            .encode_utf16()
            .collect();
        let chinese: Vec<u16> = "C:\\临时\\程序.exe\0".encode_utf16().collect();
        for (input, expected) in [
            (
                "C:\\Apps\\agentpad.exe".encode_utf16().collect::<Vec<_>>(),
                no_space,
            ),
            (Vec::new(), empty.to_vec()),
            (
                "C:\\Program Files\\AgentsPads\\agentpad.exe"
                    .encode_utf16()
                    .collect(),
                spaced,
            ),
            ("C:\\临时\\程序.exe".encode_utf16().collect(), chinese),
        ] {
            let actual = quote_windows_wide_units(&input);
            assert_eq!(actual, expected);
            assert_eq!(actual.last(), Some(&0));
            assert_eq!(actual.iter().filter(|unit| **unit == 0).count(), 1);
        }
    }

    #[test]
    fn admin_maintenance_args_are_quoted_and_parsed() {
        assert_eq!(
            join_windows_args(&[INSTALL_FLAG, r"PC\jo"]),
            r"--install-admin-task PC\jo"
        );
        assert_eq!(
            join_windows_args(&[INSTALL_FLAG, r"PC\Jo Ann"]),
            r#"--install-admin-task "PC\Jo Ann""#
        );
        assert_eq!(quote_windows_arg("a\"b"), r#""a\"b""#);
        assert_eq!(quote_windows_arg("a b\\"), r#""a b\\""#);
        assert!(matches!(
            early_action(["agentspads.exe", INSTALL_FLAG, r"PC\jo"]),
            EarlyAction::Install(user, false) if user == r"PC\jo"
        ));
        assert!(matches!(
            early_action(["agentspads.exe", INSTALL_FLAG, r"PC\jo", AUTOSTART_FLAG]),
            EarlyAction::Install(user, true) if user == r"PC\jo"
        ));
        assert!(matches!(
            early_action(["agentspads.exe", REMOVE_FLAG]),
            EarlyAction::Remove
        ));
        assert!(matches!(
            early_action(["agentspads.exe", "--elevated"]),
            EarlyAction::None
        ));
        assert!(matches!(
            early_action(["agentspads.exe", STAGE_DISABLE_FLAG, "42", "123456"]),
            EarlyAction::StageDisable(42, 123456)
        ));
        assert!(matches!(
            early_action(["agentspads.exe", DISABLE_WORKER_FLAG, "42", "123456"]),
            EarlyAction::DisableWorker(42, 123456)
        ));
        assert!(matches!(
            early_action(["agentspads.exe", DISABLE_WORKER_FLAG, "42"]),
            EarlyAction::Bad
        ));
        assert!(matches!(
            early_action(["agentspads.exe", INSTALL_FLAG]),
            EarlyAction::Bad
        ));
        assert!(matches!(
            early_action(["agentspads.exe", INSTALL_FLAG, "--elevated"]),
            EarlyAction::Bad
        ));
    }

    #[test]
    fn handoff_pid_is_parsed_without_update_semantics() {
        assert_eq!(
            handoff_parent_pid(["agentspads.exe", "--handoff", "42"]),
            Some(42)
        );
        assert_eq!(handoff_parent_pid(["agentspads.exe", "--handoff"]), None);
        assert_eq!(
            handoff_parent_pid(["agentspads.exe", "--handoff", "0"]),
            None
        );
        assert_eq!(
            handoff_parent_pid(["agentspads.exe", "--handoff", "nope"]),
            None
        );
        assert_eq!(
            handoff_parent_pid(["agentspads.exe", "--after-update", "7"]),
            None
        );
        assert_eq!(handoff_parent_pid(["agentspads.exe", "--elevated"]), None);
        assert!(matches!(
            early_action(["agentspads.exe", "--handoff", "9"]),
            EarlyAction::None
        ));
    }

    #[test]
    fn handoff_failure_reasons_stay_distinct() {
        assert_eq!(explain_handoff_failure(false, true, None), "已取消 UAC");
        assert_eq!(
            explain_handoff_failure(true, true, Some("identity store failed")),
            "已取消 UAC"
        );
        assert_eq!(
            explain_handoff_failure(true, false, Some("identity store failed: icacls failed")),
            "管理员实例未就绪：identity store failed: icacls failed"
        );
        assert_eq!(
            explain_handoff_failure(false, false, None),
            "计划任务及 UAC 回退未能启动管理员实例"
        );
        assert_eq!(
            explain_handoff_failure(true, false, None),
            "等待管理员实例就绪超时"
        );
        assert_eq!(
            explain_handoff_failure(false, false, Some("listen failed")),
            "管理员实例未就绪：listen failed"
        );
        assert_eq!(
            explain_handoff_failure(true, false, Some("  ")),
            "等待管理员实例就绪超时"
        );
    }

    #[test]
    fn observed_protected_identity_store_failure_stops_ordinary_startup() {
        use std::time::Duration;
        let notes = [
            &b"identity store failed: invalid admin identity file 2026-10-11T16:24:13Z\n"[..],
            &b"identity store failed 2026-10-11T16:24:13Z\n"[..],
        ];
        for bytes in notes {
            let note = accept_startup_error(bytes, Duration::from_secs(1)).unwrap();
            assert_eq!(
                failed_handoff_outcome(Some(&note)),
                RelaunchOutcome::ProtectedIdentityStoreFailed
            );
        }
    }

    #[test]
    fn unknown_or_ambiguous_startup_notes_do_not_fail_closed() {
        use std::time::Duration;
        let notes = [
            "identity store failedness: invalid admin identity file 2026-10-11T16:24:13Z",
            "identity store failed-extra: invalid admin identity file 2026-10-11T16:24:13Z",
            "identity store failed:invalid admin identity file 2026-10-11T16:24:13Z",
            r"identity store failed: C:\Program Files\AgentsPads 2026-10-11T16:24:13Z",
            "unknown startup failure 2026-10-11T16:24:13Z",
            "listen failed 2026-10-11T16:24:13Z",
            "identity store failed: invalid admin identity file 2026-13-99T29:61:61Z",
        ];
        for line in notes {
            let accepted = accept_startup_error(line.as_bytes(), Duration::from_secs(1)).unwrap();
            assert_eq!(
                failed_handoff_outcome(Some(&accepted)),
                RelaunchOutcome::ContinueOrdinaryStartup,
                "{line}"
            );
        }
    }

    #[test]
    fn stale_protected_identity_notes_are_filtered_before_failure_classification() {
        use std::time::{Duration, SystemTime};
        let bytes = b"identity store failed: invalid admin identity file 2026-10-11T16:24:13Z\n";
        let attempt = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let cases = [
            (
                attempt - Duration::from_secs(1),
                attempt + Duration::from_secs(1),
            ),
            (
                attempt,
                attempt + STARTUP_ERROR_MAX_AGE + Duration::from_secs(1),
            ),
        ];
        for (modified, now) in cases {
            let accepted = if startup_note_is_current(modified, now, attempt) {
                now.duration_since(modified)
                    .ok()
                    .and_then(|age| accept_startup_error(bytes, age))
            } else {
                None
            };
            assert_eq!(
                failed_handoff_outcome(accepted.as_deref()),
                RelaunchOutcome::ContinueOrdinaryStartup
            );
        }
    }

    #[test]
    fn startup_error_note_is_short_fresh_and_under_program_files() {
        use std::time::{Duration, SystemTime};
        assert_eq!(unix_utc_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(unix_utc_stamp(946_684_800), "2000-01-01T00:00:00Z");
        assert_eq!(unix_utc_stamp(1_709_210_096), "2024-02-29T12:34:56Z");
        assert_eq!(unix_utc_stamp(1_791_371_580), "2026-10-07T11:13:00Z");
        assert_eq!(
            startup_failure_text("identity store failed", "icacls failed"),
            "identity store failed: icacls failed"
        );
        assert_eq!(
            startup_failure_text("identity store failed", r"C:\Users\secret"),
            "identity store failed"
        );
        assert_eq!(
            startup_failure_text("identity store failed", "identity store failed"),
            "identity store failed"
        );
        assert_eq!(startup_failure_text("listen failed", ""), "listen failed");
        assert_eq!(
            startup_failure_text("identity store failed", &"x".repeat(81)),
            "identity store failed"
        );
        let line = startup_error_line("identity store failed: icacls failed", 946_684_800);
        assert_eq!(
            line,
            "identity store failed: icacls failed 2000-01-01T00:00:00Z\n"
        );
        assert_eq!(
            accept_startup_error(line.as_bytes(), Duration::from_secs(1)).as_deref(),
            Some("identity store failed: icacls failed 2000-01-01T00:00:00Z")
        );
        assert!(accept_startup_error(line.as_bytes(), STARTUP_ERROR_MAX_AGE).is_some());
        assert_eq!(
            accept_startup_error(
                line.as_bytes(),
                STARTUP_ERROR_MAX_AGE + Duration::from_secs(1)
            ),
            None
        );
        assert_eq!(accept_startup_error(b"", Duration::ZERO), None);
        assert_eq!(accept_startup_error(b"\n\n", Duration::ZERO), None);
        assert_eq!(accept_startup_error(&[0xff, 0xfe], Duration::ZERO), None);
        assert_eq!(
            accept_startup_error(b"ok\nignored", Duration::ZERO).as_deref(),
            Some("ok")
        );
        assert_eq!(accept_startup_error(b"bad\tline", Duration::ZERO), None);
        assert_eq!(
            accept_startup_error(&vec![b'a'; STARTUP_ERROR_MAX + 1], Duration::ZERO),
            None
        );
        assert_eq!(
            accept_startup_error(&vec![b'a'; STARTUP_ERROR_MAX], Duration::ZERO)
                .as_deref()
                .map(str::len),
            Some(STARTUP_ERROR_MAX)
        );
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert!(startup_note_is_current(t, t, t));
        assert!(!startup_note_is_current(t - Duration::from_secs(1), t, t));
        assert!(startup_note_is_current(t, t + STARTUP_ERROR_MAX_AGE, t));
        assert!(!startup_note_is_current(
            t,
            t + STARTUP_ERROR_MAX_AGE + Duration::from_secs(1),
            t
        ));
        assert!(!startup_note_is_current(t + Duration::from_secs(5), t, t));
        assert_eq!(STARTUP_ERROR_FILE, "startup_error.txt");
    }
}
