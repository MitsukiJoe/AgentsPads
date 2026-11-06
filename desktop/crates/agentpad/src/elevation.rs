use std::sync::Mutex;

static HANDOFF_FAILURE: Mutex<Option<String>> = Mutex::new(None);

pub fn handoff_failure() -> Option<String> {
    HANDOFF_FAILURE.lock().ok().and_then(|v| v.clone())
}

pub fn note_handoff_failure(reason: impl AsRef<str>) {
    if let Ok(mut value) = HANDOFF_FAILURE.lock() {
        *value = Some(reason.as_ref().into());
    }
}

pub fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub fn relaunched() -> bool {
    std::env::args().any(|a| a == "--elevated" || a == "--ready-challenge")
}

pub fn note_startup_failure(_stage: &str, _detail: &str) {
    crate::logutil::write("startup failure");
}

#[cfg(windows)]
#[path = "windows_launch.rs"]
mod windows_launch;
#[cfg(windows)]
use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
#[cfg(windows)]
use windows_launch::*;

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ProcessFacts {
    pid: u32,
    created: u64,
    session: u32,
    hwnd: isize,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ReadyRecord {
    pid: u32,
    created: u64,
    session: u32,
    hwnd: isize,
    user_sid: String,
    elevated: bool,
    image_hash: String,
    version: String,
    build: String,
    channel: String,
    challenge: String,
}

#[cfg(windows)]
pub fn installed_executable() -> Result<PathBuf, String> {
    protected_target().map(|(_, exe)| exe)
}

#[cfg(windows)]
pub fn installed() -> bool {
    validate_install().is_ok()
        && std::env::current_exe()
            .ok()
            .zip(installed_executable().ok())
            .is_some_and(|(a, b)| same_file(&a, &b))
}

#[cfg(windows)]
pub(crate) fn validate_install() -> Result<PathBuf, String> {
    let exe = installed_executable()?;
    let root = exe.parent().ok_or("安装目录无效")?;
    let marker = root.join("managed-install.json");
    crate::managed_install::validate_parent_directories(root)?;
    for path in [root, &exe, &marker] {
        if path.exists() {
            crate::managed_tasks::validate_install_acl(path)?;
        }
    }
    if !no_reparse_ancestors(&marker) || !no_reparse_ancestors(&exe) {
        return Err("请先使用安装包安装 AgentsPads，再使用管理员入口或更新".into());
    }
    let content = read_small(&marker, 2048).ok_or("缺少安装记录，请重新运行安装包修复")?;
    let value: serde_json::Value = serde_json::from_slice(&content).map_err(|_| "安装记录损坏")?;
    if value["schema"] != 1
        || value["product"] != "AgentsPads"
        || value["channel"] != env!("AGENTPAD_CHANNEL")
    {
        return Err("安装记录或发布渠道不匹配，请使用对应安装包修复".into());
    }
    Ok(exe)
}

#[cfg(windows)]
pub fn image_hash() -> Result<String, String> {
    static VALUE: std::sync::OnceLock<Result<String, String>> = std::sync::OnceLock::new();
    VALUE
        .get_or_init(|| {
            use sha2::{Digest, Sha256};
            use std::io::Read;
            let mut file = std::fs::File::open(std::env::current_exe().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            let mut hasher = Sha256::new();
            let mut buffer = [0u8; 65536];
            loop {
                let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            Ok(format!("{:x}", hasher.finalize()))
        })
        .clone()
}

#[cfg(windows)]
pub fn enabled() -> bool {
    crate::managed_tasks::enabled()
}

#[cfg(windows)]
pub fn protected_state_dir() -> Option<PathBuf> {
    let exe = installed_executable().ok()?;
    let sid = crate::managed_tasks::current_user_sid().ok()?;
    let path = exe.parent()?.join("state").join(sid);
    no_reparse_ancestors(&path).then_some(path)
}

#[cfg(windows)]
pub fn admin_identity_path() -> std::io::Result<PathBuf> {
    if !is_elevated() || !installed() {
        return Err(std::io::Error::other(
            "administrator identity requires the installed application",
        ));
    }
    let dir = protected_state_dir()
        .ok_or_else(|| std::io::Error::other("protected state unavailable"))?;
    prepare_admin_identity_at(&dir.join("secret"))
}

#[cfg(windows)]
pub struct OperationLock {
    _file: std::fs::File,
    _mutex: OwnedHandle,
}

#[cfg(windows)]
pub fn operation_lock() -> Result<OperationLock, String> {
    maintenance_lock(false, false)
}

#[cfg(windows)]
fn maintenance_lock(allow_installer: bool, read_only: bool) -> Result<OperationLock, String> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::System::Threading::CreateMutexW;
    if !allow_installer && installer_running() {
        return Err("安装或卸载程序正在运行，请完成后再操作。".into());
    }
    let exe = validate_install()?;
    if !read_only && !is_elevated() {
        return Err("维护正式程序需要管理员权限".into());
    }
    let path = exe.parent().ok_or("安装目录无效")?.join("operation.lock");
    if !no_reparse_ancestors(&path) {
        return Err("维护锁路径不安全".into());
    }
    let file = std::fs::OpenOptions::new()
        .create(!read_only)
        .truncate(false)
        .read(true)
        .write(!read_only)
        .share_mode(0)
        .open(path)
        .map_err(|_| "另一个更新、卸载或权限切换正在进行，请稍后重试")?;
    let raw = unsafe {
        CreateMutexW(
            None,
            false,
            windows::core::w!("Global\\AgentsPads.Managed.Maintenance"),
        )
    }
    .map_err(|e| e.to_string())?;
    let mutex = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    if !allow_installer && installer_running() {
        return Err("安装或卸载程序正在运行，请完成后再操作。".into());
    }
    Ok(OperationLock {
        _file: file,
        _mutex: mutex,
    })
}

#[cfg(windows)]
pub fn launch_same_user(
    exe: &Path,
    args: &[String],
    elevated: bool,
) -> Result<OwnedHandle, String> {
    crate::managed_tasks::validate_desktop_user()?;
    let installed = validate_install()?;
    let worker = installed
        .parent()
        .ok_or("安装目录无效")?
        .join("updates/update-worker.exe");
    if (!same_file(exe, &installed) && !same_file(exe, &worker))
        || !exe.is_file()
        || !no_reparse_ancestors(exe)
    {
        return Err("只允许启动受保护安装目录中的程序".into());
    }
    if same_file(exe, &worker) {
        crate::managed_tasks::validate_install_acl(worker.parent().ok_or("更新目录无效")?)?;
        crate::managed_tasks::validate_install_acl(&worker)?;
    }
    if args.iter().any(|s| s.contains('\0')) {
        return Err("启动参数无效".into());
    }
    if elevated && !is_elevated() {
        return shell_exec(exe.as_os_str(), &join_args(args), false).map_err(|e| match e {
            LaunchError::Cancelled => "已取消系统授权，原程序保持运行".into(),
            LaunchError::Failed => "无法启动管理员程序".into(),
        });
    }
    if !elevated && is_elevated() {
        return launch_medium(exe, args);
    }
    let child = std::process::Command::new(exe)
        .args(args)
        .current_dir(exe.parent().ok_or("安装目录无效")?)
        .spawn()
        .map_err(|e| format!("无法启动程序：{e}"))?;
    Ok(child.into())
}

#[cfg(windows)]
pub fn process_creation_time(pid: u32) -> Result<u64, String> {
    windows_launch::process_creation_time(pid)
}

#[cfg(windows)]
fn session() -> Result<u32, String> {
    let mut value = 0;
    unsafe {
        windows::Win32::System::RemoteDesktop::ProcessIdToSessionId(std::process::id(), &mut value)
    }
    .map_err(|e| e.to_string())?;
    Ok(value)
}

#[cfg(windows)]
pub fn validate_process(pid: u32, elevated: bool) -> Result<(), String> {
    let (facts, actual, path) = process_facts(pid, 0).ok_or("无法验证运行实例")?;
    if actual != elevated
        || facts.session != session()?
        || !same_file(&path, &installed_executable()?)
        || crate::managed_tasks::process_user_sid(pid)? != crate::managed_tasks::current_user_sid()?
    {
        return Err("运行实例的路径、账户、会话或权限不匹配".into());
    }
    Ok(())
}

#[cfg(windows)]
fn ready_path(elevated: bool) -> Option<PathBuf> {
    let root = if elevated {
        protected_state_dir()?
    } else {
        crate::identity::fixed_user_data_dir().join("runtime")
    };
    let file = root.join(format!("ready-{}.json", session().ok()?));
    no_reparse_ancestors(&file).then_some(file)
}

#[cfg(windows)]
fn read_small(path: &Path, cap: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || !no_reparse_ancestors(path) || meta.len() > cap {
        return None;
    }
    let mut data = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(cap + 1)
        .read_to_end(&mut data)
        .ok()?;
    (data.len() <= cap as usize).then_some(data)
}

#[cfg(windows)]
fn read_ready(elevated: bool) -> Option<ReadyRecord> {
    serde_json::from_slice(&read_small(&ready_path(elevated)?, 8192)?).ok()
}

fn ready_identity_matches(
    record: &ReadyRecord,
    hash: &str,
    elevated: bool,
    challenge: &str,
) -> bool {
    record.pid != 0
        && record.created != 0
        && record.hwnd != 0
        && record.image_hash == hash
        && hash.len() == 64
        && hash.bytes().all(|b| b.is_ascii_hexdigit())
        && record.elevated == elevated
        && record.channel == env!("AGENTPAD_CHANNEL")
        && !record.build.is_empty()
        && !record.version.is_empty()
        && (challenge.is_empty() || record.challenge == challenge)
}

#[cfg(windows)]
pub fn verify_ready(pid: u32, hash: &str, elevated: bool, challenge: &str) -> bool {
    let Some(record) = read_ready(elevated) else {
        return false;
    };
    if record.pid != pid || !ready_identity_matches(&record, hash, elevated, challenge) {
        return false;
    }
    let Some((facts, actual, path)) = process_facts(pid, record.hwnd) else {
        return false;
    };
    facts.created == record.created
        && facts.session == record.session
        && actual == elevated
        && session().ok() == Some(facts.session)
        && crate::managed_tasks::current_user_sid().ok().as_ref() == Some(&record.user_sid)
        && crate::managed_tasks::process_user_sid(pid).ok().as_ref() == Some(&record.user_sid)
        && installed_executable().is_ok_and(|exe| same_file(&path, &exe))
        && no_reparse_ancestors(&path)
        && window_belongs_to(record.hwnd, pid)
        && window_responding(record.hwnd)
        && owns_listener(pid)
}

#[cfg(windows)]
fn window_responding(hwnd: isize) -> bool {
    use windows::Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        UI::WindowsAndMessaging::{SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_NULL},
    };
    unsafe {
        SendMessageTimeoutW(
            HWND(hwnd as *mut _),
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            1000,
            None,
        )
        .0 != 0
    }
}

#[cfg(windows)]
pub fn publish_ready(hwnd: isize) -> std::io::Result<()> {
    if !installed() {
        return Ok(());
    }
    let fail = || std::io::Error::other("running application failed readiness validation");
    let pid = std::process::id();
    let (facts, elevated, _) = process_facts(pid, hwnd).ok_or_else(fail)?;
    if !owns_listener(pid) || !window_belongs_to(hwnd, pid) {
        return Err(fail());
    }
    let record = ReadyRecord {
        pid,
        created: facts.created,
        session: facts.session,
        hwnd,
        user_sid: crate::managed_tasks::current_user_sid().map_err(std::io::Error::other)?,
        elevated,
        image_hash: image_hash().map_err(std::io::Error::other)?,
        version: crate::updater::CURRENT_VERSION.to_string(),
        build: env!("AGENTPAD_BUILD_ID").into(),
        channel: env!("AGENTPAD_CHANNEL").into(),
        challenge: argument_after("--ready-challenge").unwrap_or_default(),
    };
    let path = ready_path(elevated).ok_or_else(fail)?;
    std::fs::create_dir_all(path.parent().ok_or_else(fail)?)?;
    std::fs::write(path, serde_json::to_vec(&record)?)
}

#[cfg(windows)]
fn focus(record: &ReadyRecord) {
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{SetForegroundWindow, ShowWindowAsync, SW_RESTORE},
    };
    unsafe {
        let hwnd = HWND(record.hwnd as *mut _);
        let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        let _ = SetForegroundWindow(hwnd);
    }
}

#[cfg(windows)]
fn wait_ready(
    hash: &str,
    elevated: bool,
    challenge: &str,
    expected: Option<u32>,
) -> Result<ReadyRecord, String> {
    let until = Instant::now() + Duration::from_secs(30);
    while Instant::now() < until {
        if let Some(record) = read_ready(elevated) {
            if expected.is_none_or(|pid| pid == record.pid)
                && verify_ready(record.pid, hash, elevated, challenge)
            {
                focus(&record);
                return Ok(record);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("程序尚未就绪，未确认切换成功。请检查窗口或安全软件提示后重试".into())
}

#[cfg(windows)]
fn argument_after(flag: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == flag {
            return args.next();
        }
    }
    None
}

#[cfg(windows)]
fn join_args(args: &[String]) -> String {
    args.iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_arg(arg: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        if c == '\"' {
            out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat_n('\\', slashes));
        }
        slashes = 0;
        out.push(c);
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('\"');
    out
}

#[cfg(windows)]
fn event_name(pid: u32, created: u64) -> String {
    format!("Local\\AgentsPads.Managed.Close.{pid}.{created}")
}

#[cfg(windows)]
static CLOSE_EVENT: std::sync::OnceLock<OwnedHandle> = std::sync::OnceLock::new();

#[cfg(windows)]
static APP_MUTEX: std::sync::OnceLock<OwnedHandle> = std::sync::OnceLock::new();

#[cfg(windows)]
fn claim_app_slot() -> Result<(), String> {
    use windows::Win32::{
        Foundation::{GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, WIN32_ERROR},
        System::Threading::{CreateMutexW, OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE},
    };
    match unsafe {
        OpenMutexW(
            SYNCHRONIZATION_SYNCHRONIZE,
            false,
            windows::core::w!("Global\\AgentsPads.Managed.Install"),
        )
    } {
        Ok(handle) => {
            drop(unsafe { OwnedHandle::from_raw_handle(handle.0) });
            return Err("安装程序正在运行，请完成安装后再打开 AgentsPads。".into());
        }
        Err(e) if WIN32_ERROR::from_error(&e) == Some(ERROR_FILE_NOT_FOUND) => {}
        Err(_) => return Err("无法确认安装程序状态，请完成安装后再启动。".into()),
    }
    let handle = unsafe {
        CreateMutexW(
            None,
            false,
            windows::core::w!("Global\\AgentsPads.Managed.Running"),
        )
    }
    .map_err(|error| {
        format!("无法建立程序运行状态，请确认其他 AgentsPads 实例已退出后重试：{error}")
    })?;
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
    if existed {
        return Err(
            "已有 AgentsPads 正在启动或运行，请从托盘检查。其他账户或会话运行的实例需要先退出。"
                .into(),
        );
    }
    APP_MUTEX
        .set(handle)
        .map_err(|_| "运行控制项重复初始化".into())
}

#[cfg(windows)]
fn installer_running() -> bool {
    use windows::Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR},
        System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE},
    };
    match unsafe {
        OpenMutexW(
            SYNCHRONIZATION_SYNCHRONIZE,
            false,
            windows::core::w!("Global\\AgentsPads.Managed.Install"),
        )
    } {
        Ok(h) => {
            drop(unsafe { OwnedHandle::from_raw_handle(h.0) });
            true
        }
        Err(e) => WIN32_ERROR::from_error(&e) != Some(ERROR_FILE_NOT_FOUND),
    }
}

#[cfg(windows)]
fn create_close_event() -> Result<(), String> {
    use windows::{
        core::HSTRING,
        Win32::{
            Foundation::{GetLastError, LocalFree, ERROR_ALREADY_EXISTS, HLOCAL},
            Security::{
                Authorization::{
                    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
                },
                PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
            },
            System::Threading::CreateEventW,
        },
    };
    let name = HSTRING::from(event_name(
        std::process::id(),
        process_creation_time(std::process::id())?,
    ));
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let mut attr = SECURITY_ATTRIBUTES::default();
    if is_elevated() {
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                windows::core::w!("D:P(A;;GA;;;SY)(A;;GA;;;BA)"),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
        }
        .map_err(|e| e.to_string())?;
        attr.nLength = std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32;
        attr.lpSecurityDescriptor = sd.0;
    }
    let result = unsafe {
        CreateEventW(
            if is_elevated() { Some(&attr) } else { None },
            true,
            false,
            &name,
        )
    };
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if !sd.0.is_null() {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(sd.0)));
        }
    }
    let handle = result.map_err(|e| e.to_string())?;
    let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
    if existed {
        return Err("程序的关闭控制项已被占用，请重新启动".into());
    }
    CLOSE_EVENT
        .set(handle)
        .map_err(|_| "关闭控制项重复初始化".to_string())
}

#[cfg(windows)]
pub fn close_requested() -> bool {
    use windows::Win32::{
        Foundation::{HANDLE, WAIT_OBJECT_0},
        System::Threading::WaitForSingleObject,
    };
    CLOSE_EVENT.get().is_some_and(|h| unsafe {
        WaitForSingleObject(HANDLE(h.as_raw_handle()), 0) == WAIT_OBJECT_0
    })
}

#[cfg(windows)]
pub fn request_close(pid: u32) -> Result<(), String> {
    use windows::{
        core::HSTRING,
        Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE},
    };
    let (_, actual, _) = process_facts(pid, 0).ok_or("目标程序已退出或不可验证")?;
    validate_process(pid, actual)?;
    let name = HSTRING::from(event_name(pid, process_creation_time(pid)?));
    let raw = unsafe { OpenEventW(EVENT_MODIFY_STATE, false, &name) }
        .map_err(|_| "程序不接受安全关闭请求；请手动从托盘退出后重试")?;
    let owned = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    unsafe { SetEvent(windows::Win32::Foundation::HANDLE(owned.as_raw_handle())) }
        .map_err(|e| e.to_string())
}

#[cfg(windows)]
fn parent_process(pid: u32, created: u64) -> Result<OwnedHandle, String> {
    use windows::Win32::{
        Foundation::FILETIME,
        System::Threading::{
            GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        },
    };
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
    }
    .map_err(|_| "无法等待原实例退出，本次切换未开始")?;
    let process = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let mut start = FILETIME::default();
    let mut end = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { GetProcessTimes(raw, &mut start, &mut end, &mut kernel, &mut user) }
        .map_err(|_| "无法确认原实例，本次切换未开始")?;
    if (u64::from(start.dwHighDateTime) << 32) | u64::from(start.dwLowDateTime) != created {
        return Err("原实例已改变，本次切换未开始".into());
    }
    Ok(process)
}

#[cfg(windows)]
fn wait_exit(process: &OwnedHandle) -> Result<(), String> {
    use windows::Win32::{
        Foundation::{HANDLE, WAIT_OBJECT_0},
        System::Threading::WaitForSingleObject,
    };
    if unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), 20_000) } == WAIT_OBJECT_0 {
        Ok(())
    } else {
        Err("旧程序尚未完全退出，本次操作已停止".into())
    }
}

#[cfg(windows)]
pub fn begin_switch(elevated: bool) -> Result<(), String> {
    if elevated == is_elevated() {
        return Ok(());
    }
    let exe = validate_install()?;
    if elevated && !enabled() {
        install_task(false)?;
    }
    let args = vec![
        "--switch-permission".into(),
        if elevated { "admin" } else { "normal" }.into(),
        std::process::id().to_string(),
        process_creation_time(std::process::id())?.to_string(),
        image_hash()?,
    ];
    launch_same_user(&exe, &args, is_elevated())?;
    Ok(())
}

#[cfg(windows)]
fn switch_worker(args: &[String]) -> Result<(), String> {
    if args.len() != 5 || !matches!(args[1].as_str(), "normal" | "admin") {
        return Err("权限切换参数无效".into());
    }
    let desired = args[1] == "admin";
    let previous = is_elevated();
    let pid = args[2].parse::<u32>().map_err(|_| "进程参数无效")?;
    let created = args[3].parse::<u64>().map_err(|_| "进程参数无效")?;
    let hash = image_hash()?;
    if hash != args[4] || desired == previous || process_creation_time(pid)? != created {
        return Err("权限切换构建或进程不匹配".into());
    }
    validate_process(pid, previous)?;
    let parent = parent_process(pid, created)?;
    let _lock = maintenance_lock(false, !is_elevated())?;
    let exe = validate_install()?;
    if desired && !enabled() {
        return Err("管理员入口尚未授权".into());
    }
    request_close(pid)?;
    wait_exit(&parent)?;
    let mut child: Option<OwnedHandle> = None;
    let mut task: Option<crate::managed_tasks::RunningLaunch> = None;
    let launched = (|| -> Result<(), String> {
        if desired {
            task = Some(crate::managed_tasks::run_tracked()?);
            wait_ready(&hash, true, "", None)?;
        } else {
            let challenge = uuid::Uuid::new_v4().to_string();
            child = Some(launch_same_user(
                &exe,
                &["--ready-challenge".into(), challenge.clone()],
                false,
            )?);
            let pid = unsafe {
                windows::Win32::System::Threading::GetProcessId(windows::Win32::Foundation::HANDLE(
                    child.as_ref().unwrap().as_raw_handle(),
                ))
            };
            wait_ready(&hash, false, &challenge, Some(pid))?;
        }
        Ok(())
    })();
    if let Err(reason) = launched {
        let stopped = if let Some(child) = &child {
            stop_spawned(child)
        } else if let Some(task) = &task {
            task.stop()
        } else {
            Ok(())
        };
        if let Err(error) = stopped {
            return Err(format!("{reason}。新实例未能退出：{error}。程序版本未改变，请从托盘退出后重新打开需要的入口。"));
        }
        let challenge = uuid::Uuid::new_v4().to_string();
        let mut restore = vec!["--ready-challenge".into(), challenge.clone()];
        if previous {
            restore.push("--elevated".into());
        }
        let restored = launch_same_user(&exe, &restore, previous).and_then(|child| {
            let pid = unsafe {
                windows::Win32::System::Threading::GetProcessId(windows::Win32::Foundation::HANDLE(
                    child.as_raw_handle(),
                ))
            };
            wait_ready(&hash, previous, &challenge, Some(pid)).map(|_| ())
        });
        return Err(if restored.is_ok() {
            format!("{reason}。已恢复原来的运行权限。")
        } else {
            format!("{reason}。自动恢复未成功，正式程序文件仍保留，请从开始菜单重新打开。")
        });
    }
    Ok(())
}

#[cfg(windows)]
fn stop_spawned(child: &OwnedHandle) -> Result<(), String> {
    use windows::Win32::{
        Foundation::{HANDLE, WAIT_OBJECT_0},
        System::Threading::{GetProcessId, TerminateProcess, WaitForSingleObject},
    };
    let handle = HANDLE(child.as_raw_handle());
    if unsafe { WaitForSingleObject(handle, 0) } == WAIT_OBJECT_0 {
        return Ok(());
    }
    let _ = request_close(unsafe { GetProcessId(handle) });
    if unsafe { WaitForSingleObject(handle, 3000) } == WAIT_OBJECT_0 {
        return Ok(());
    }
    unsafe { TerminateProcess(handle, 1) }.map_err(|e| e.to_string())?;
    if unsafe { WaitForSingleObject(handle, 5000) } == WAIT_OBJECT_0 {
        Ok(())
    } else {
        Err("未就绪的新实例仍未退出".into())
    }
}

#[cfg(windows)]
pub fn install_task(_autostart: bool) -> Result<(), String> {
    if enabled() {
        return Ok(());
    }
    let exe = validate_install()?;
    shell_exec_wait(exe.as_os_str(), "--register-admin-launch").map_err(String::from)?;
    if !enabled() {
        return Err("管理员入口未完成授权；请检查系统是否支持当前账户的静默管理员启动".into());
    }
    Ok(())
}

#[cfg(windows)]
pub fn set_login_autostart(value: bool) -> Result<(), String> {
    if is_elevated() {
        let _lock = operation_lock()?;
        return crate::managed_tasks::set_login_startup(value, true);
    }
    let exe = validate_install()?;
    let argument = if value {
        "--set-autostart normal"
    } else {
        "--set-autostart off"
    };
    shell_exec_wait(exe.as_os_str(), argument).map_err(String::from)
}

#[cfg(windows)]
pub fn exit_if_admin_maintenance() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(flag) = args.first().map(String::as_str) else {
        return;
    };
    let result = match flag {
        "--register-admin-launch" if args.len() == 1 => (|| {
            let _lock = maintenance_lock(true, false)?;
            crate::managed_tasks::register()
        })(),
        "--unregister-admin-launch" if args.len() == 1 => (|| {
            let _lock = maintenance_lock(true, false)?;
            crate::managed_tasks::unregister_all()
        })(),
        "--launch-admin" if args.len() == 1 => launch_admin_entry(),
        "--switch-permission" => switch_worker(&args),
        "--set-autostart"
            if args.len() == 2 && matches!(args[1].as_str(), "normal" | "admin" | "off") =>
        {
            (|| {
                let _lock = operation_lock()?;
                crate::managed_tasks::set_login_startup(args[1] != "off", args[1] == "admin")
            })()
        }
        _ => return,
    };
    if let Err(reason) = result {
        show_error(&reason);
        std::process::exit(1);
    }
    std::process::exit(0);
}

#[cfg(not(windows))]
pub fn exit_if_admin_maintenance() {}

#[cfg(windows)]
fn launch_admin_entry() -> Result<(), String> {
    if installer_running() {
        return Err("安装或卸载尚未完成，请稍后再启动。".into());
    }
    if !installed() {
        return Err("请从已安装的管理员快捷方式启动".into());
    }
    let hash = image_hash()?;
    if let Some(record) = read_ready(true) {
        if verify_ready(record.pid, &hash, true, "") {
            focus(&record);
            return Ok(());
        }
    }
    if let Some(record) = read_ready(false) {
        if verify_ready(record.pid, &hash, false, "") {
            focus(&record);
            return Err("普通实例正在运行，请在现有窗口中切换到管理员权限，或从托盘退出后再打开管理员入口。".into());
        }
    }
    if !enabled() {
        install_task(false)?;
    }
    crate::managed_tasks::run()?;
    wait_ready(&hash, true, "", None)?;
    Ok(())
}

#[cfg(windows)]
pub fn prepare_startup() -> Result<bool, String> {
    image_hash()?;
    if relaunched() && !installed() {
        return Err("管理员入口和更新交接只允许启动已安装的正式程序".into());
    }
    if std::env::args().any(|s| s == "--elevated") && !is_elevated() {
        return Err(
            "系统没有授予管理员权限。当前账户或 Windows 管理员保护可能不支持此启动方式。".into(),
        );
    }
    if std::env::args().any(|s| s == "--login-startup") && is_elevated() {
        return Err("普通登录入口得到了非预期的管理员权限，已停止启动。".into());
    }
    if is_elevated() {
        if !installed() {
            return Err(
                "请先安装，再通过管理员入口启动；下载副本不能作为管理员正式程序运行。".into(),
            );
        }
        crate::managed_tasks::validate_desktop_user()?;
    }
    if installed() {
        let hash = image_hash()?;
        if let Some(record) = read_ready(is_elevated()) {
            if verify_ready(record.pid, &hash, is_elevated(), "") {
                focus(&record);
                return Ok(false);
            }
        }
        if let Some(record) = read_ready(!is_elevated()) {
            if verify_ready(record.pid, &hash, !is_elevated(), "") {
                focus(&record);
                return Err("另一个权限的实例正在运行。请在现有窗口切换运行权限，或先从托盘退出，再使用对应入口。".into());
            }
        }
        claim_app_slot()?;
        create_close_event()?;
    }
    Ok(true)
}

#[cfg(not(windows))]
pub fn prepare_startup() -> Result<bool, String> {
    Ok(true)
}

#[cfg(windows)]
pub fn show_error(text: &str) {
    let text = windows::core::HSTRING::from(text);
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
            None,
            &text,
            windows::core::w!("AgentsPads"),
            windows::Win32::UI::WindowsAndMessaging::MB_OK
                | windows::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ready() -> ReadyRecord {
        ReadyRecord {
            pid: 1,
            created: 2,
            session: 3,
            hwnd: 4,
            user_sid: "S-1-5-21-1".into(),
            elevated: true,
            image_hash: "a".repeat(64),
            version: "1.0".into(),
            build: "commit-one".into(),
            channel: env!("AGENTPAD_CHANNEL").into(),
            challenge: "attempt-one".into(),
        }
    }
    #[test]
    fn same_version_different_image_is_rejected() {
        let r = ready();
        assert!(!ready_identity_matches(
            &r,
            &"b".repeat(64),
            true,
            "attempt-one"
        ));
    }
    #[test]
    fn exact_image_permission_and_attempt_are_required() {
        let r = ready();
        assert!(ready_identity_matches(
            &r,
            &r.image_hash,
            true,
            "attempt-one"
        ));
        assert!(!ready_identity_matches(
            &r,
            &r.image_hash,
            false,
            "attempt-one"
        ));
        assert!(!ready_identity_matches(
            &r,
            &r.image_hash,
            true,
            "old-attempt"
        ));
    }
    #[test]
    fn other_channel_is_rejected() {
        let mut r = ready();
        r.channel = "preview".into();
        assert!(!ready_identity_matches(
            &r,
            &r.image_hash,
            true,
            "attempt-one"
        ));
    }
    #[test]
    fn windows_arguments_quote_backslashes_and_quotes() {
        assert_eq!(quote_arg("C:\\a b\\"), "\"C:\\a b\\\\\"");
        assert_eq!(quote_arg("a\"b"), "\"a\\\"b\"");
    }
}
