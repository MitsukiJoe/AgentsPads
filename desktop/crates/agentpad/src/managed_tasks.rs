#![cfg(windows)]

use std::ffi::{c_void, OsString};
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};

use windows::core::{Interface, BSTR, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    LocalFree, HANDLE, HLOCAL, RPC_E_CHANGED_MODE, SCHED_E_TASK_NOT_RUNNING, VARIANT_BOOL,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    ConvertStringSidToSidW, GetNamedSecurityInfoW, SetNamedSecurityInfoW, SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    AclSizeInformation, GetAce, GetAclInformation, GetSecurityDescriptorControl,
    GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, GetTokenInformation, IsValidSid,
    LookupAccountNameW, TokenElevation, TokenSessionId, TokenUser, ACCESS_ALLOWED_ACE, ACE_HEADER,
    ACL, ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION, INHERIT_ONLY_ACE,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SE_DACL_PROTECTED, SID_NAME_USE, TOKEN_ELEVATION, TOKEN_INFORMATION_CLASS, TOKEN_QUERY,
    TOKEN_USER,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::SystemInformation::GetWindowsDirectoryW;
use windows::Win32::System::TaskScheduler::{
    IExecAction, ILogonTrigger, IRegisteredTask, IRunningTask, ITaskDefinition, ITaskFolder,
    ITaskService, TaskScheduler, TASK_ACTION_EXEC, TASK_CREATE_OR_UPDATE,
    TASK_DONT_ADD_PRINCIPAL_ACE, TASK_ENUM_HIDDEN, TASK_IGNORE_REGISTRATION_TRIGGERS,
    TASK_INSTANCES_PARALLEL, TASK_LOGON_INTERACTIVE_TOKEN, TASK_RUNLEVEL_HIGHEST,
    TASK_RUNLEVEL_LUA, TASK_RUN_USER_SID, TASK_RUN_USE_SESSION_ID, TASK_STATE_DISABLED,
    TASK_STATE_READY, TASK_TRIGGER_LOGON,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken,
    QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};

const PREFIX: &str = "AgentsPads.Managed.";
const ARGUMENTS: &str = "--elevated";
const LOGIN_ARGUMENTS: &str = "--login-startup";
const SYSTEM_SID: &str = "S-1-5-18";
const ADMINISTRATORS_SID: &str = "S-1-5-32-544";
const TRUSTED_INSTALLER_SID: &str =
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
const FILE_ALL_ACCESS: u32 = 0x001f_01ff;
const FILE_READ_EXECUTE: u32 = 0x0012_00a9;
const OWNER_AND_DACL: i32 = 0x0000_0005;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Admin,
    Autostart,
}

impl Kind {
    fn name(self, sid: &str) -> String {
        let suffix = match self {
            Self::Admin => "Admin",
            Self::Autostart => "Autostart",
        };
        format!("{PREFIX}{sid}.{suffix}")
    }
}

fn scoped_task_name(name: &str) -> Option<(String, Kind)> {
    let name = name.strip_prefix(PREFIX)?;
    let (sid, kind) = if let Some(sid) = name.strip_suffix(".Autostart") {
        (sid, Kind::Autostart)
    } else {
        (name.strip_suffix(".Admin")?, Kind::Admin)
    };
    if !sid.starts_with("S-1-")
        || sid
            .bytes()
            .any(|b| b != b'S' && b != b'-' && !b.is_ascii_digit())
    {
        return None;
    }
    let wide: Vec<u16> = sid.encode_utf16().chain(Some(0)).collect();
    let mut parsed = PSID::default();
    unsafe { ConvertStringSidToSidW(PCWSTR(wide.as_ptr()), &mut parsed) }.ok()?;
    let _allocation = LocalAllocation(parsed.0);
    (unsafe { sid_text(parsed) }.ok()?.as_str() == sid).then(|| (sid.into(), kind))
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(Some(HLOCAL(self.0))) };
        }
    }
}

struct ComApartment(bool);

impl ComApartment {
    fn initialize() -> Result<Self, String> {
        let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if initialized.is_err() && initialized != RPC_E_CHANGED_MODE {
            return Err(format!(
                "无法连接 Windows 计划任务（0x{:08X}）",
                initialized.0 as u32
            ));
        }
        Ok(Self(initialized.is_ok()))
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn error(context: &str, error: windows::core::Error) -> String {
    format!("{context}（0x{:08X}）", error.code().0 as u32)
}

fn token_for(process: HANDLE) -> Result<OwnedHandle, String> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }
        .map_err(|e| error("无法读取账户身份", e))?;
    Ok(unsafe { OwnedHandle::from_raw_handle(token.0) })
}

fn current_token() -> Result<OwnedHandle, String> {
    token_for(unsafe { GetCurrentProcess() })
}

fn token_info(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Result<Vec<usize>, String> {
    let mut needed = 0u32;
    unsafe { GetTokenInformation(token, class, None, 0, &mut needed) }.ok();
    if needed == 0 || needed > 64 * 1024 {
        return Err("无法读取账户身份".into());
    }
    let mut storage = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    unsafe {
        GetTokenInformation(
            token,
            class,
            Some(storage.as_mut_ptr().cast()),
            needed,
            &mut needed,
        )
    }
    .map_err(|e| error("无法读取账户身份", e))?;
    Ok(storage)
}

unsafe fn sid_text(sid: PSID) -> Result<String, String> {
    if !unsafe { IsValidSid(sid) }.as_bool() {
        return Err("账户标识无效".into());
    }
    let mut text = PWSTR::null();
    unsafe { ConvertSidToStringSidW(sid, &mut text) }.map_err(|e| error("无法读取账户标识", e))?;
    let _allocation = LocalAllocation(text.0.cast());
    unsafe { text.to_string() }.map_err(|_| "账户标识无效".into())
}

fn account_sid(account: &str) -> Option<String> {
    if account.is_empty() || account.len() > 32768 || account.contains('\0') {
        return None;
    }
    let wide: Vec<u16> = account.encode_utf16().chain(Some(0)).collect();
    if account.starts_with("S-1-") {
        let mut sid = PSID::default();
        unsafe { ConvertStringSidToSidW(PCWSTR(wide.as_ptr()), &mut sid) }.ok()?;
        let _allocation = LocalAllocation(sid.0);
        return unsafe { sid_text(sid) }.ok();
    }
    let mut sid_bytes = 0u32;
    let mut domain_chars = 0u32;
    let mut use_type = SID_NAME_USE::default();
    unsafe {
        LookupAccountNameW(
            None,
            PCWSTR(wide.as_ptr()),
            None,
            &mut sid_bytes,
            None,
            &mut domain_chars,
            &mut use_type,
        )
    }
    .ok();
    if sid_bytes == 0 || sid_bytes > 4096 || domain_chars > 32768 {
        return None;
    }
    let mut sid_storage = vec![0usize; (sid_bytes as usize).div_ceil(size_of::<usize>())];
    let mut domain = vec![0u16; domain_chars.max(1) as usize];
    let sid = PSID(sid_storage.as_mut_ptr().cast());
    unsafe {
        LookupAccountNameW(
            None,
            PCWSTR(wide.as_ptr()),
            Some(sid),
            &mut sid_bytes,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut domain_chars,
            &mut use_type,
        )
    }
    .ok()?;
    unsafe { sid_text(sid) }.ok()
}

fn token_sid(token: HANDLE) -> Result<String, String> {
    let info = token_info(token, TokenUser)?;
    let user = unsafe { &*info.as_ptr().cast::<TOKEN_USER>() };
    unsafe { sid_text(user.User.Sid) }
}

fn token_session(token: HANDLE) -> Result<u32, String> {
    let info = token_info(token, TokenSessionId)?;
    Ok(unsafe { *info.as_ptr().cast::<u32>() })
}

fn token_elevated(token: HANDLE) -> Result<bool, String> {
    let info = token_info(token, TokenElevation)?;
    Ok(unsafe { (*info.as_ptr().cast::<TOKEN_ELEVATION>()).TokenIsElevated != 0 })
}

fn session_for(pid: u32) -> Result<u32, String> {
    let mut session = 0;
    unsafe { ProcessIdToSessionId(pid, &mut session) }.map_err(|e| error("无法验证登录会话", e))?;
    if session == 0 {
        return Err("管理员启动需要已登录的桌面会话".into());
    }
    Ok(session)
}

pub fn current_user_sid() -> Result<String, String> {
    let token = current_token()?;
    token_sid(HANDLE(token.as_raw_handle()))
}

pub fn process_user_sid(pid: u32) -> Result<String, String> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|e| error("无法读取运行进程身份", e))?;
    let process = unsafe { OwnedHandle::from_raw_handle(process.0) };
    let token = token_for(HANDLE(process.as_raw_handle()))?;
    token_sid(HANDLE(token.as_raw_handle()))
}

fn process_image(process: HANDLE) -> Result<PathBuf, String> {
    let mut buffer = vec![0u16; 32768];
    let mut length = buffer.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    }
    .map_err(|e| error("无法验证桌面程序", e))?;
    Ok(PathBuf::from(OsString::from_wide(
        &buffer[..length as usize],
    )))
}

fn explorer_path() -> Result<PathBuf, String> {
    let mut buffer = vec![0u16; 32768];
    let length = unsafe { GetWindowsDirectoryW(Some(&mut buffer)) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err("无法验证 Windows 桌面位置".into());
    }
    Ok(PathBuf::from(OsString::from_wide(&buffer[..length])).join("explorer.exe"))
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
}

pub fn validate_desktop_user() -> Result<(), String> {
    let current = current_token()?;
    let current = HANDLE(current.as_raw_handle());
    let session = session_for(unsafe { GetCurrentProcessId() })?;
    let shell = unsafe { GetShellWindow() };
    if shell.0.is_null() {
        return Err("当前没有可验证的 Windows 桌面，请登录桌面后重试".into());
    }
    let mut pid = 0;
    if unsafe { GetWindowThreadProcessId(shell, Some(&mut pid)) } == 0 || pid == 0 {
        return Err("无法验证当前 Windows 桌面".into());
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|e| error("无法验证当前 Windows 桌面", e))?;
    let process = unsafe { OwnedHandle::from_raw_handle(process.0) };
    let process = HANDLE(process.as_raw_handle());
    let shell_token = token_for(process)?;
    let shell_token = HANDLE(shell_token.as_raw_handle());
    if session_for(pid)? != session
        || token_session(current)? != session
        || token_session(shell_token)? != session
        || token_sid(current)? != token_sid(shell_token)?
        || token_elevated(shell_token)?
        || !same_path(&process_image(process)?, &explorer_path()?)
    {
        return Err(
            "此管理员授权不属于当前桌面账户；不支持使用另一账户或隔离管理员身份建立静默启动".into(),
        );
    }
    Ok(())
}

fn require_admin() -> Result<(), String> {
    let token = current_token()?;
    if !token_elevated(HANDLE(token.as_raw_handle()))? {
        return Err("修改管理员启动授权需要管理员权限".into());
    }
    validate_desktop_user()
}

fn with_folder<T>(operation: impl FnOnce(&ITaskFolder) -> Result<T, String>) -> Result<T, String> {
    let _apartment = ComApartment::initialize()?;
    let service: ITaskService =
        unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| error("无法连接 Windows 计划任务", e))?;
    let empty = VARIANT::default();
    unsafe { service.Connect(&empty, &empty, &empty, &empty) }
        .map_err(|e| error("无法连接 Windows 计划任务", e))?;
    let folder = unsafe { service.GetFolder(&BSTR::from("\\")) }
        .map_err(|e| error("无法读取 Windows 计划任务", e))?;
    operation(&folder)
}

fn task(folder: &ITaskFolder, name: &str) -> Result<Option<IRegisteredTask>, String> {
    match unsafe { folder.GetTask(&BSTR::from(name)) } {
        Ok(task) => Ok(Some(task)),
        Err(e) if matches!(e.code().0 as u32, 0x8007_0002 | 0x8007_0003) => Ok(None),
        Err(e) => Err(error("无法读取管理员启动配置", e)),
    }
}

fn task_sddl(sid: &str) -> String {
    format!("O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FRFX;;;{sid})")
}

fn validate_security_descriptor(sddl: &str, user_sid: &str) -> Result<(), String> {
    let wide: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(wide.as_ptr()),
            1,
            &mut descriptor,
            None,
        )
    }
    .map_err(|e| error("无法验证管理员启动权限", e))?;
    let _allocation = LocalAllocation(descriptor.0);
    let mut owner = PSID::default();
    let mut defaulted = windows::core::BOOL::default();
    let mut control = 0u16;
    let mut revision = 0u32;
    let mut present = windows::core::BOOL::default();
    let mut acl: *mut ACL = std::ptr::null_mut();
    unsafe {
        GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted)
            .map_err(|e| error("无法验证管理员启动所有者", e))?;
        GetSecurityDescriptorControl(descriptor, &mut control, &mut revision)
            .map_err(|e| error("无法验证管理员启动权限", e))?;
        GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted)
            .map_err(|e| error("无法验证管理员启动权限", e))?;
    }
    let owner = unsafe { sid_text(owner) }?;
    if !matches!(owner.as_str(), ADMINISTRATORS_SID | SYSTEM_SID)
        || control & SE_DACL_PROTECTED.0 == 0
        || !present.as_bool()
        || acl.is_null()
    {
        return Err("管理员启动权限不安全，请通过安装程序修复".into());
    }
    let mut info = ACL_SIZE_INFORMATION::default();
    unsafe {
        GetAclInformation(
            acl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    }
    .map_err(|e| error("无法验证管理员启动权限", e))?;
    if info.AceCount != 3 {
        return Err("管理员启动包含非预期权限，请通过安装程序修复".into());
    }
    let mut found = [false; 3];
    for index in 0..info.AceCount {
        let mut ace: *mut c_void = std::ptr::null_mut();
        unsafe { GetAce(acl, index, &mut ace) }.map_err(|e| error("无法验证管理员启动权限", e))?;
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if header.AceType != 0
            || header.AceFlags != 0
            || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
        {
            return Err("管理员启动包含非预期权限，请通过安装程序修复".into());
        }
        let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let sid = unsafe { sid_text(PSID((&ace.SidStart as *const u32).cast_mut().cast())) }?;
        let (position, mask) = match sid.as_str() {
            SYSTEM_SID => (0, FILE_ALL_ACCESS),
            ADMINISTRATORS_SID => (1, FILE_ALL_ACCESS),
            value if value == user_sid => (2, FILE_READ_EXECUTE),
            _ => return Err("管理员启动向其他账户开放了权限，请通过安装程序修复".into()),
        };
        if found[position] || ace.Mask != mask {
            return Err("管理员启动权限不符合预期，请通过安装程序修复".into());
        }
        found[position] = true;
    }
    if found != [true; 3] {
        return Err("管理员启动权限不完整，请通过安装程序修复".into());
    }
    Ok(())
}

fn trusted_install_owner(sid: &str) -> bool {
    matches!(sid, SYSTEM_SID | ADMINISTRATORS_SID | TRUSTED_INSTALLER_SID)
}

fn validate_install_descriptor(descriptor: PSECURITY_DESCRIPTOR) -> Result<(), String> {
    const MUTATION_RIGHTS: u32 = 0x0000_0002
        | 0x0000_0004
        | 0x0000_0010
        | 0x0000_0040
        | 0x0000_0100
        | 0x0001_0000
        | SECURITY_MUTATION_RIGHTS;
    validate_descriptor_mutation(descriptor, MUTATION_RIGHTS)
}

const SECURITY_MUTATION_RIGHTS: u32 =
    0x0004_0000 | 0x0008_0000 | 0x0100_0000 | 0x0200_0000 | 0x1000_0000 | 0x4000_0000;

fn ancestor_mutation_rights(volume_root: bool) -> u32 {
    SECURITY_MUTATION_RIGHTS | 0x0000_0040 | if volume_root { 0 } else { 0x0001_0110 }
}

pub fn validate_registry_descriptor(descriptor: PSECURITY_DESCRIPTOR) -> Result<(), String> {
    let mut control = 0u16;
    let mut revision = 0u32;
    unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) }
        .map_err(|e| error("无法验证安装位置记录的权限", e))?;
    if control & SE_DACL_PROTECTED.0 == 0 {
        return Err("安装位置记录的权限未受保护，请使用安装程序修复".into());
    }
    validate_descriptor_mutation(
        descriptor,
        SECURITY_MUTATION_RIGHTS | 0x0001_0000 | 0x0000_0002 | 0x0000_0004 | 0x0000_0020,
    )
}

fn validate_descriptor_mutation(
    descriptor: PSECURITY_DESCRIPTOR,
    mutation_rights: u32,
) -> Result<(), String> {
    let mut owner = PSID::default();
    let mut defaulted = windows::core::BOOL::default();
    let mut present = windows::core::BOOL::default();
    let mut acl: *mut ACL = std::ptr::null_mut();
    unsafe {
        GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted)
            .map_err(|e| error("无法验证安装文件所有者", e))?;
        GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted)
            .map_err(|e| error("无法验证安装文件权限", e))?;
    }
    if !trusted_install_owner(&unsafe { sid_text(owner) }?) || !present.as_bool() || acl.is_null() {
        return Err("正式安装目录或文件的所有者、访问权限不安全，请用安装程序修复".into());
    }
    let mut info = ACL_SIZE_INFORMATION::default();
    unsafe {
        GetAclInformation(
            acl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    }
    .map_err(|e| error("无法验证安装文件权限", e))?;
    for index in 0..info.AceCount {
        let mut ace: *mut c_void = std::ptr::null_mut();
        unsafe { GetAce(acl, index, &mut ace) }.map_err(|e| error("无法验证安装文件权限", e))?;
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if u32::from(header.AceFlags) & INHERIT_ONLY_ACE.0 != 0 {
            continue;
        }
        match header.AceType {
            1 => continue,
            0 if usize::from(header.AceSize) >= size_of::<ACCESS_ALLOWED_ACE>() => {}
            _ => return Err("正式安装目录或文件包含无法安全确认的权限，请用安装程序修复".into()),
        }
        let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        if ace.Mask & mutation_rights == 0 {
            continue;
        }
        let sid = unsafe { sid_text(PSID((&ace.SidStart as *const u32).cast_mut().cast())) }?;
        if !trusted_install_owner(&sid) {
            return Err(
                "普通账户可修改正式安装目录或文件，已停止高权限操作；请用安装程序修复".into(),
            );
        }
    }
    Ok(())
}

fn with_file_security(
    path: &Path,
    check: impl FnOnce(PSECURITY_DESCRIPTOR) -> Result<(), String>,
) -> Result<(), String> {
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    if path[..path.len() - 1].contains(&0) {
        return Err("正式安装路径无效".into());
    }
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let result = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(path.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            None,
            None,
            None,
            None,
            &mut descriptor,
        )
    };
    let _allocation = LocalAllocation(descriptor.0);
    result.ok().map_err(|e| error("无法读取正式安装权限", e))?;
    check(descriptor)
}

pub fn validate_install_acl(path: &Path) -> Result<(), String> {
    with_file_security(path, validate_install_descriptor)
}

pub fn validate_install_ancestor(path: &Path, volume_root: bool) -> Result<(), String> {
    with_file_security(path, |descriptor| {
        validate_descriptor_mutation(descriptor, ancestor_mutation_rights(volume_root))
    })
}

fn secret_sddl(directory: bool) -> String {
    let flags = if directory { "OICI" } else { "" };
    format!("O:BAD:P(A;{flags};FA;;;SY)(A;{flags};FA;;;BA)")
}

fn validate_secret_descriptor(
    descriptor: PSECURITY_DESCRIPTOR,
    directory: bool,
) -> Result<(), String> {
    let mut owner = PSID::default();
    let mut defaulted = windows::core::BOOL::default();
    let mut present = windows::core::BOOL::default();
    let mut control = 0u16;
    let mut revision = 0u32;
    let mut acl: *mut ACL = std::ptr::null_mut();
    unsafe {
        GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted)
            .map_err(|e| error("无法验证管理员密钥所有者", e))?;
        GetSecurityDescriptorControl(descriptor, &mut control, &mut revision)
            .map_err(|e| error("无法验证管理员密钥权限", e))?;
        GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted)
            .map_err(|e| error("无法验证管理员密钥权限", e))?;
    }
    if unsafe { sid_text(owner) }? != ADMINISTRATORS_SID
        || control & SE_DACL_PROTECTED.0 == 0
        || !present.as_bool()
        || acl.is_null()
    {
        return Err("管理员密钥权限不安全，已停止使用".into());
    }
    let mut info = ACL_SIZE_INFORMATION::default();
    unsafe {
        GetAclInformation(
            acl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    }
    .map_err(|e| error("无法验证管理员密钥权限", e))?;
    if info.AceCount != 2 {
        return Err("管理员密钥仍允许其他账户访问，已停止使用".into());
    }
    let mut found = [false; 2];
    for index in 0..info.AceCount {
        let mut ace: *mut c_void = std::ptr::null_mut();
        unsafe { GetAce(acl, index, &mut ace) }.map_err(|e| error("无法验证管理员密钥权限", e))?;
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if header.AceType != 0
            || header.AceFlags != if directory { 3 } else { 0 }
            || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
        {
            return Err("管理员密钥包含非预期权限，已停止使用".into());
        }
        let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let sid = unsafe { sid_text(PSID((&ace.SidStart as *const u32).cast_mut().cast())) }?;
        let position = match sid.as_str() {
            SYSTEM_SID => 0,
            ADMINISTRATORS_SID => 1,
            _ => return Err("管理员密钥允许其他账户访问，已停止使用".into()),
        };
        if found[position] || ace.Mask != FILE_ALL_ACCESS {
            return Err("管理员密钥权限不完整，已停止使用".into());
        }
        found[position] = true;
    }
    if found != [true; 2] {
        return Err("管理员密钥权限不完整，已停止使用".into());
    }
    Ok(())
}

pub fn protect_secret_acl(path: &Path) -> Result<(), String> {
    use std::os::windows::fs::MetadataExt;
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| "无法读取管理员密钥位置".to_string())?;
    if metadata.file_attributes() & 0x400 != 0 || (!metadata.is_dir() && !metadata.is_file()) {
        return Err("管理员密钥位置不安全".into());
    }
    let wide: Vec<u16> = secret_sddl(metadata.is_dir())
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(wide.as_ptr()),
            1,
            &mut descriptor,
            None,
        )
    }
    .map_err(|e| error("无法准备管理员密钥权限", e))?;
    let _allocation = LocalAllocation(descriptor.0);
    let mut owner = PSID::default();
    let mut defaulted = windows::core::BOOL::default();
    let mut present = windows::core::BOOL::default();
    let mut acl: *mut ACL = std::ptr::null_mut();
    unsafe {
        GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted)
            .map_err(|e| error("无法准备管理员密钥所有者", e))?;
        GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted)
            .map_err(|e| error("无法准备管理员密钥权限", e))?;
    }
    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    if path_wide[..path_wide.len() - 1].contains(&0) {
        return Err("管理员密钥路径无效".into());
    }
    unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(path_wide.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            Some(owner),
            None,
            Some(acl),
            None,
        )
    }
    .ok()
    .map_err(|e| error("无法保护管理员密钥", e))?;
    with_file_security(path, |descriptor| {
        validate_secret_descriptor(descriptor, metadata.is_dir())
    })
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn task_xml(sid: &str, executable: &Path, kind: Kind, elevated: bool) -> Result<String, String> {
    if kind == Kind::Admin && !elevated {
        return Err("管理员入口不能注册为普通启动".into());
    }
    let executable = executable.to_str().ok_or("正式安装路径无效")?;
    if executable.contains('\0') || executable.contains('%') || executable.contains("$(") {
        return Err("正式安装路径不能包含计划任务替换变量".into());
    }
    let working = Path::new(executable)
        .parent()
        .and_then(Path::to_str)
        .ok_or("正式安装路径无效")?;
    let triggers = if kind == Kind::Autostart {
        format!("<Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{}</UserId><Delay>PT30S</Delay></LogonTrigger></Triggers>", xml_escape(sid))
    } else {
        "<Triggers/>".into()
    };
    let (level, arguments) = if elevated {
        ("HighestAvailable", ARGUMENTS)
    } else {
        ("LeastPrivilege", LOGIN_ARGUMENTS)
    };
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?><Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\"><RegistrationInfo><Description>AgentsPads managed launch</Description></RegistrationInfo>{triggers}<Principals><Principal id=\"Owner\"><UserId>{sid}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>{level}</RunLevel></Principal></Principals><Settings><MultipleInstancesPolicy>Parallel</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><AllowHardTerminate>true</AllowHardTerminate><StartWhenAvailable>false</StartWhenAvailable><AllowStartOnDemand>true</AllowStartOnDemand><Enabled>true</Enabled><ExecutionTimeLimit>PT0S</ExecutionTimeLimit></Settings><Actions Context=\"Owner\"><Exec><Command>{executable}</Command><Arguments>{arguments}</Arguments><WorkingDirectory>{working}</WorkingDirectory></Exec></Actions></Task>",
        sid = xml_escape(sid), executable = xml_escape(executable), working = xml_escape(working),
    ))
}

fn validate_task_definition(
    task: &IRegisteredTask,
    sid: &str,
    kind: Kind,
    elevated: bool,
) -> Result<(), String> {
    let expected = crate::elevation::installed_executable()?;
    let definition =
        unsafe { task.Definition() }.map_err(|e| error("无法读取管理员启动配置", e))?;
    validate_definition(&definition, sid, kind, elevated, &expected)
}

fn validate_definition(
    definition: &ITaskDefinition,
    sid: &str,
    kind: Kind,
    elevated: bool,
    expected: &Path,
) -> Result<(), String> {
    if kind == Kind::Admin && !elevated {
        return Err("管理员入口的权限不符合预期".into());
    }
    let checked = || -> windows::core::Result<Option<&'static str>> {
        unsafe {
            let principal = definition.Principal()?;
            let mut user = BSTR::default();
            let mut group = BSTR::default();
            let mut logon = Default::default();
            let mut level = Default::default();
            principal.UserId(&mut user)?;
            principal.GroupId(&mut group)?;
            principal.LogonType(&mut logon)?;
            principal.RunLevel(&mut level)?;
            if account_sid(&user.to_string()).as_deref() != Some(sid) {
                return Ok(Some("运行账户"));
            }
            if !group.is_empty() {
                return Ok(Some("运行账户组"));
            }
            if logon != TASK_LOGON_INTERACTIVE_TOKEN {
                return Ok(Some("登录方式"));
            }
            if level
                != if elevated {
                    TASK_RUNLEVEL_HIGHEST
                } else {
                    TASK_RUNLEVEL_LUA
                }
            {
                return Ok(Some("运行权限"));
            }
            let actions = definition.Actions()?;
            let mut count = 0;
            actions.Count(&mut count)?;
            if count != 1 {
                return Ok(Some("启动动作数量"));
            }
            let action = actions.get_Item(1)?;
            let mut action_type = Default::default();
            action.Type(&mut action_type)?;
            if action_type != TASK_ACTION_EXEC {
                return Ok(Some("启动动作类型"));
            }
            let action: IExecAction = action.cast()?;
            let mut executable = BSTR::default();
            let mut arguments = BSTR::default();
            let mut working = BSTR::default();
            action.Path(&mut executable)?;
            action.Arguments(&mut arguments)?;
            action.WorkingDirectory(&mut working)?;
            if !same_path(Path::new(&executable.to_string()), expected) {
                return Ok(Some("程序路径"));
            }
            if arguments != if elevated { ARGUMENTS } else { LOGIN_ARGUMENTS } {
                return Ok(Some("启动参数"));
            }
            if expected
                .parent()
                .is_none_or(|parent| !same_path(Path::new(&working.to_string()), parent))
            {
                return Ok(Some("工作目录"));
            }
            let settings = definition.Settings()?;
            let mut demand = VARIANT_BOOL::default();
            let mut hard_terminate = VARIANT_BOOL::default();
            let mut battery_stop = VARIANT_BOOL::default();
            let mut time_limit = BSTR::default();
            let mut parallel = Default::default();
            settings.AllowDemandStart(&mut demand)?;
            settings.AllowHardTerminate(&mut hard_terminate)?;
            settings.StopIfGoingOnBatteries(&mut battery_stop)?;
            settings.ExecutionTimeLimit(&mut time_limit)?;
            settings.MultipleInstances(&mut parallel)?;
            if !demand.as_bool() {
                return Ok(Some("手动启动"));
            }
            if !hard_terminate.as_bool() {
                return Ok(Some("停止方式"));
            }
            if battery_stop.as_bool() {
                return Ok(Some("电池停止设置"));
            }
            if time_limit != "PT0S" {
                return Ok(Some("运行时限"));
            }
            if parallel != TASK_INSTANCES_PARALLEL {
                return Ok(Some("并发方式"));
            }
            let triggers = definition.Triggers()?;
            triggers.Count(&mut count)?;
            if kind == Kind::Admin {
                return Ok((count != 0).then_some("管理员入口触发器"));
            }
            if count != 1 {
                return Ok(Some("登录触发器数量"));
            }
            let trigger = triggers.get_Item(1)?;
            let mut trigger_type = Default::default();
            let mut trigger_enabled = VARIANT_BOOL::default();
            trigger.Type(&mut trigger_type)?;
            trigger.Enabled(&mut trigger_enabled)?;
            if trigger_type != TASK_TRIGGER_LOGON {
                return Ok(Some("登录触发器类型"));
            }
            if !trigger_enabled.as_bool() {
                return Ok(Some("登录触发器状态"));
            }
            let trigger: ILogonTrigger = trigger.cast()?;
            let mut user = BSTR::default();
            trigger.UserId(&mut user)?;
            Ok((account_sid(&user.to_string()).as_deref() != Some(sid)).then_some("登录触发账户"))
        }
    };
    if let Some(field) = checked().map_err(|e| error("无法验证管理员启动配置", e))? {
        return Err(format!(
            "管理员启动配置不一致（{field}），请通过安装程序修复"
        ));
    }
    Ok(())
}

fn validate_task(
    task: &IRegisteredTask,
    sid: &str,
    kind: Kind,
    elevated: bool,
) -> Result<(), String> {
    validate_task_definition(task, sid, kind, elevated)?;
    let descriptor = unsafe { task.GetSecurityDescriptor(OWNER_AND_DACL) }
        .map_err(|e| error("无法验证管理员启动权限", e))?;
    validate_security_descriptor(&descriptor.to_string(), sid)
}

fn registered_elevation(task: &IRegisteredTask) -> Result<bool, String> {
    let mut level = Default::default();
    unsafe {
        task.Definition()
            .and_then(|definition| definition.Principal())
            .and_then(|principal| principal.RunLevel(&mut level))
    }
    .map_err(|e| error("无法验证登录启动权限", e))?;
    if level == TASK_RUNLEVEL_HIGHEST {
        Ok(true)
    } else if level == TASK_RUNLEVEL_LUA {
        Ok(false)
    } else {
        Err("登录启动权限不符合预期".into())
    }
}

fn ensure_registered(
    folder: &ITaskFolder,
    sid: &str,
    kind: Kind,
    elevated: bool,
) -> Result<(), String> {
    let name = kind.name(sid);
    if let Some(existing) = task(folder, &name)? {
        let existing_mode = registered_elevation(&existing)?;
        validate_task(&existing, sid, kind, existing_mode)?;
    }
    let executable = crate::elevation::validate_install()?;
    if !executable.is_file() {
        return Err("找不到正式安装的程序，请先重新安装 AgentsPads".into());
    }
    let xml = task_xml(sid, &executable, kind, elevated)?;
    let user = VARIANT::from(sid);
    let descriptor = VARIANT::from(task_sddl(sid).as_str());
    let registered = unsafe {
        folder.RegisterTask(
            &BSTR::from(name.as_str()),
            &BSTR::from(xml.as_str()),
            TASK_CREATE_OR_UPDATE.0
                | TASK_DONT_ADD_PRINCIPAL_ACE.0
                | TASK_IGNORE_REGISTRATION_TRIGGERS.0,
            &user,
            &VARIANT::default(),
            TASK_LOGON_INTERACTIVE_TOKEN,
            &descriptor,
        )
    }
    .map_err(|e| error("无法保存管理员启动授权", e))?;
    if let Err(reason) = validate_task(&registered, sid, kind, elevated) {
        if unsafe { folder.DeleteTask(&BSTR::from(name.as_str()), 0) }.is_err() {
            return Err(format!(
                "{reason}；新建任务未能撤销，请在安装程序中执行修复或卸载"
            ));
        }
        return Err(reason);
    }
    Ok(())
}

fn remove_task(folder: &ITaskFolder, sid: &str, kind: Kind) -> Result<(), String> {
    let name = kind.name(sid);
    if task(folder, &name)?.is_none() {
        return Ok(());
    }
    unsafe { folder.DeleteTask(&BSTR::from(name.as_str()), 0) }
        .map_err(|e| error("无法撤销管理员启动任务", e))?;
    if task(folder, &name)?.is_some() {
        return Err("管理员启动任务仍然存在".into());
    }
    Ok(())
}

fn kind_enabled(kind: Kind) -> Result<bool, String> {
    let sid = current_user_sid()?;
    with_folder(|folder| {
        let Some(task) = task(folder, &kind.name(&sid))? else {
            return Ok(false);
        };
        validate_task(&task, &sid, kind, true)?;
        unsafe { task.Enabled() }
            .map(|value| value.as_bool())
            .map_err(|e| error("无法读取管理员启动状态", e))
    })
}

pub fn enabled() -> bool {
    kind_enabled(Kind::Admin).unwrap_or(false)
}

pub fn register() -> Result<(), String> {
    require_admin()?;
    let sid = current_user_sid()?;
    with_folder(|folder| ensure_registered(folder, &sid, Kind::Admin, true))
}

pub fn unregister_all() -> Result<(), String> {
    require_admin()?;
    with_folder(|folder| {
        let tasks = unsafe { folder.GetTasks(TASK_ENUM_HIDDEN.0) }
            .map_err(|e| error("无法读取已安装的管理员启动任务", e))?;
        let count =
            unsafe { tasks.Count() }.map_err(|e| error("无法读取已安装的管理员启动任务", e))?;
        let mut candidates = Vec::new();
        for index in 1..=count {
            let registered = unsafe { tasks.get_Item(&VARIANT::from(index)) }
                .map_err(|e| error("无法读取已安装的管理员启动任务", e))?;
            let name =
                unsafe { registered.Name() }.map_err(|e| error("无法读取管理员启动任务名称", e))?;
            if let Some(scoped) = scoped_task_name(&name.to_string()) {
                candidates.push(scoped);
            }
        }
        let mut failures = Vec::new();
        for (sid, kind) in candidates {
            let remove = || {
                let Some(registered) = task(folder, &kind.name(&sid))? else {
                    return Ok(());
                };
                let elevated = registered_elevation(&registered)?;
                validate_task_definition(&registered, &sid, kind, elevated)?;
                remove_task(folder, &sid, kind)
            };
            if let Err(reason) = remove() {
                failures.push(reason);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("；"))
        }
    })
}

pub struct RunningLaunch {
    task: IRunningTask,
    _apartment: ComApartment,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl RunningLaunch {
    pub fn stop(&self) -> Result<(), String> {
        match unsafe { self.task.Stop() } {
            Ok(()) => {}
            Err(e) if e.code() == SCHED_E_TASK_NOT_RUNNING => return Ok(()),
            Err(e) => {
                return Err(error(
                    "无法停止本次管理员启动，请从托盘或任务管理器退出后重试",
                    e,
                ))
            }
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            match unsafe { self.task.State() } {
                Ok(TASK_STATE_READY | TASK_STATE_DISABLED) => return Ok(()),
                Err(e) if e.code() == SCHED_E_TASK_NOT_RUNNING => return Ok(()),
                Err(e) => return Err(error("无法确认本次管理员实例已经停止", e)),
                _ => {}
            }
            if std::time::Instant::now() >= deadline {
                return Err("本次管理员实例尚未停止，暂不恢复其他实例；请先手动退出该程序".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

pub fn run_tracked() -> Result<RunningLaunch, String> {
    validate_desktop_user()?;
    crate::elevation::validate_install()?;
    let sid = current_user_sid()?;
    let session = session_for(unsafe { GetCurrentProcessId() })?;
    let session = i32::try_from(session).map_err(|_| "登录会话编号无效")?;
    let apartment = ComApartment::initialize()?;
    let running = with_folder(|folder| {
        let task = task(folder, &Kind::Admin.name(&sid))?.ok_or("此账户尚未授权管理员启动")?;
        validate_task(&task, &sid, Kind::Admin, true)?;
        if !unsafe { task.Enabled() }
            .map_err(|e| error("无法读取管理员启动状态", e))?
            .as_bool()
        {
            return Err("管理员启动授权已被禁用，请通过安装程序重新授权".into());
        }
        unsafe {
            task.RunEx(
                &VARIANT::default(),
                TASK_RUN_USE_SESSION_ID.0 | TASK_RUN_USER_SID.0,
                session,
                &BSTR::from(sid.as_str()),
            )
        }
        .map_err(|e| error("Windows 未能在当前桌面启动管理员实例", e))
    })?;
    Ok(RunningLaunch {
        task: running,
        _apartment: apartment,
        _thread_bound: std::marker::PhantomData,
    })
}

pub fn run() -> Result<(), String> {
    run_tracked().map(|_| ())
}

pub fn login_startup_mode() -> Option<bool> {
    let sid = current_user_sid().ok()?;
    with_folder(|folder| {
        let Some(task) = task(folder, &Kind::Autostart.name(&sid))? else {
            return Ok(None);
        };
        let elevated = registered_elevation(&task)?;
        validate_task(&task, &sid, Kind::Autostart, elevated)?;
        if !unsafe { task.Enabled() }
            .map_err(|e| error("无法读取登录启动状态", e))?
            .as_bool()
        {
            return Ok(None);
        }
        Ok(Some(elevated))
    })
    .ok()
    .flatten()
}

pub fn set_login_startup(enabled: bool, elevated: bool) -> Result<(), String> {
    require_admin()?;
    let sid = current_user_sid()?;
    with_folder(|folder| {
        if !enabled {
            return remove_task(folder, &sid, Kind::Autostart);
        }
        if elevated {
            let admin = task(folder, &Kind::Admin.name(&sid))?.ok_or("请先授权管理员启动")?;
            validate_task(&admin, &sid, Kind::Admin, true)?;
            if !unsafe { admin.Enabled() }
                .map_err(|e| error("无法读取管理员启动状态", e))?
                .as_bool()
            {
                return Err("请先启用管理员启动授权".into());
            }
        }
        ensure_registered(folder, &sid, Kind::Autostart, elevated)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "S-1-5-21-100-200-300-1001";

    fn check_descriptor(
        sddl: &str,
        check: impl FnOnce(PSECURITY_DESCRIPTOR) -> Result<(), String>,
    ) -> Result<(), String> {
        let wide: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide.as_ptr()),
                1,
                &mut descriptor,
                None,
            )
        }
        .expect("test security descriptor must be valid SDDL");
        let _allocation = LocalAllocation(descriptor.0);
        check(descriptor)
    }

    fn check_install_descriptor(sddl: &str) -> Result<(), String> {
        check_descriptor(sddl, validate_install_descriptor)
    }

    #[test]
    fn secret_acl_requires_exact_privileged_owner_and_entries() {
        for directory in [false, true] {
            assert!(check_descriptor(&secret_sddl(directory), |descriptor| {
                validate_secret_descriptor(descriptor, directory)
            })
            .is_ok());
        }
        let wrong_owner = secret_sddl(false).replace("O:BA", &format!("O:{USER}"));
        assert!(check_descriptor(&wrong_owner, |descriptor| {
            validate_secret_descriptor(descriptor, false)
        })
        .is_err());
    }

    #[test]
    fn secret_acl_rejects_extra_readers_and_inherited_permissions() {
        for sddl in [
            format!("{}(A;;FR;;;{USER})", secret_sddl(false)),
            secret_sddl(false).replace("D:P", "D:"),
            secret_sddl(false).replace("(A;;FA;;;BA)", "(A;;FA;;;SY)"),
        ] {
            assert!(check_descriptor(&sddl, |descriptor| {
                validate_secret_descriptor(descriptor, false)
            })
            .is_err());
        }
    }

    #[test]
    fn install_acl_accepts_protected_files_and_default_program_files_permissions() {
        assert!(
            check_install_descriptor("O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FRFX;;;BU)").is_ok()
        );
        let program_files = format!("O:{TRUSTED_INSTALLER_SID}G:BAD:PAI(A;OICIIO;GA;;;CO)(A;OICIIO;GA;;;SY)(A;;0x1301bf;;;SY)(A;OICIIO;GA;;;BA)(A;;0x1301bf;;;BA)(A;;FRFX;;;BU)(A;;FA;;;{TRUSTED_INSTALLER_SID})(A;;FRFX;;;AC)");
        assert!(check_install_descriptor(&program_files).is_ok());
    }

    #[test]
    fn install_acl_rejects_user_ownership_and_unrestricted_access() {
        assert!(check_install_descriptor(&format!("O:{USER}G:BAD:P(A;;FA;;;BA)")).is_err());
        assert!(check_install_descriptor("O:BAG:BAD:NO_ACCESS_CONTROL").is_err());
        assert!(check_install_descriptor("O:BAG:BAD:P(A;;FA;;;WD)").is_err());
    }

    #[test]
    fn install_acl_rejects_every_untrusted_mutation_right() {
        for right in [
            "GW", "GA", "WD", "WO", "SD", "0x2", "0x4", "0x10", "0x40", "0x100",
        ] {
            let sddl = format!("O:BAG:BAD:P(A;;FA;;;BA)(A;;{right};;;{USER})");
            assert!(check_install_descriptor(&sddl).is_err(), "{right}");
        }
        let inherited = format!("O:BAG:BAD:P(A;;FA;;;BA)(A;ID;GW;;;{USER})");
        assert!(check_install_descriptor(&inherited).is_err());
    }

    #[test]
    fn install_acl_ignores_only_inheritance_and_deny_entries() {
        let sddl =
            format!("O:BAG:BAD:P(D;;GW;;;{USER})(A;OICIIO;GA;;;CO)(A;;FA;;;BA)(A;;FRFX;;;{USER})");
        assert!(check_install_descriptor(&sddl).is_ok());
        let conditional =
            format!("O:BAG:BAD:P(A;;FA;;;BA)(XA;;GW;;;{USER};(@User.Title == \"test\"))");
        assert!(check_install_descriptor(&conditional).is_err());
    }

    #[test]
    fn ancestors_allow_creating_siblings_but_not_replacing_the_installed_directory() {
        for volume_root in [false, true] {
            for rights in ["FRFX", "0x2", "0x4", "0x6"] {
                let sddl = format!("O:BAG:BAD:P(A;;FA;;;BA)(A;;{rights};;;{USER})");
                assert!(
                    check_descriptor(&sddl, |descriptor| {
                        validate_descriptor_mutation(
                            descriptor,
                            ancestor_mutation_rights(volume_root),
                        )
                    })
                    .is_ok(),
                    "{rights}, root={volume_root}"
                );
            }
            for rights in ["0x40", "WD", "WO", "GW", "GA", "0x1000000", "0x2000000"] {
                let sddl = format!("O:BAG:BAD:P(A;;FA;;;BA)(A;;{rights};;;{USER})");
                assert!(
                    check_descriptor(&sddl, |descriptor| {
                        validate_descriptor_mutation(
                            descriptor,
                            ancestor_mutation_rights(volume_root),
                        )
                    })
                    .is_err(),
                    "{rights}, root={volume_root}"
                );
            }
            let user_owned = format!("O:{USER}G:BAD:P(A;;FA;;;BA)");
            assert!(check_descriptor(&user_owned, |descriptor| {
                validate_descriptor_mutation(descriptor, ancestor_mutation_rights(volume_root))
            })
            .is_err());
        }
        for rights in ["SD", "0x10", "0x100"] {
            let sddl = format!("O:BAG:BAD:P(A;;FA;;;BA)(A;;{rights};;;{USER})");
            assert!(
                check_descriptor(&sddl, |descriptor| {
                    validate_descriptor_mutation(descriptor, ancestor_mutation_rights(false))
                })
                .is_err(),
                "{rights}"
            );
            assert!(
                check_descriptor(&sddl, |descriptor| {
                    validate_descriptor_mutation(descriptor, ancestor_mutation_rights(true))
                })
                .is_ok(),
                "{rights}"
            );
        }
    }

    #[test]
    fn registry_registration_allows_reading_but_rejects_user_mutation_and_ownership() {
        assert!(check_descriptor(
            "O:BAG:BAD:P(A;;KA;;;BA)(A;;KR;;;BU)",
            validate_registry_descriptor
        )
        .is_ok());
        for rights in [
            "0x2",
            "0x4",
            "0x20",
            "SD",
            "WD",
            "WO",
            "GW",
            "GA",
            "KW",
            "KA",
            "0x1000000",
            "0x2000000",
        ] {
            let sddl = format!("O:BAG:BAD:P(A;;KA;;;BA)(A;;{rights};;;{USER})");
            assert!(
                check_descriptor(&sddl, validate_registry_descriptor).is_err(),
                "{rights}"
            );
        }
        let user_owned = format!("O:{USER}G:BAD:P(A;;KA;;;BA)");
        assert!(check_descriptor(&user_owned, validate_registry_descriptor).is_err());
        assert!(check_descriptor(
            "O:BAG:BAD:(A;;KA;;;BA)(A;;KR;;;BU)",
            validate_registry_descriptor,
        )
        .is_err());
    }

    #[test]
    fn task_acl_grants_user_only_read_and_execute() {
        assert!(validate_security_descriptor(&task_sddl(USER), USER).is_ok());
        let writable = task_sddl(USER).replace("FRFX", "FA");
        assert!(validate_security_descriptor(&writable, USER).is_err());
    }

    #[test]
    fn task_acl_rejects_other_users_and_unprotected_inheritance() {
        let other_user = task_sddl("S-1-5-21-100-200-300-1002");
        assert!(validate_security_descriptor(&other_user, USER).is_err());
        assert!(validate_security_descriptor(&task_sddl(USER).replace("D:P", "D:"), USER).is_err());
        let everyone = format!("{}(A;;FA;;;WD)", task_sddl(USER));
        assert!(validate_security_descriptor(&everyone, USER).is_err());
    }

    #[test]
    fn task_acl_rejects_user_owned_definition() {
        let descriptor = task_sddl(USER).replace("O:BA", &format!("O:{USER}"));
        assert!(validate_security_descriptor(&descriptor, USER).is_err());
    }

    #[test]
    fn task_acl_rejects_missing_or_duplicate_system_permissions() {
        let descriptor = task_sddl(USER).replace("(A;;FA;;;SY)", "");
        assert!(validate_security_descriptor(&descriptor, USER).is_err());
        let descriptor = task_sddl(USER).replace("(A;;FA;;;BA)", "(A;;FA;;;SY)");
        assert!(validate_security_descriptor(&descriptor, USER).is_err());
    }

    #[test]
    fn tasks_are_scoped_to_account_and_purpose() {
        assert_ne!(Kind::Admin.name(USER), Kind::Autostart.name(USER));
        assert_ne!(
            Kind::Admin.name(USER),
            Kind::Admin.name("S-1-5-21-100-200-300-1002")
        );
        assert!(!Kind::Admin.name(USER).contains("\\"));
        assert_eq!(
            scoped_task_name(&Kind::Admin.name(USER)),
            Some((USER.into(), Kind::Admin))
        );
        assert_eq!(
            scoped_task_name(&Kind::Autostart.name(USER)),
            Some((USER.into(), Kind::Autostart))
        );
        for name in [
            "AgentsPads Elevated",
            "Other.AgentsPads.Managed.S-1-5-21-100-200-300-1001.Admin",
            "AgentsPads.Managed.not-a-sid.Admin",
            "AgentsPads.Managed.S-1-5-21-100-200-300-1001.Admin.Other",
            "AgentsPads.Managed.S-1-5-21-100-200-300-1001.Unrelated",
        ] {
            assert!(scoped_task_name(name).is_none());
        }
    }

    #[test]
    fn task_actions_cannot_expand_environment_or_runtime_parameters() {
        for path in [r"C:\%TEMP%\agentspads.exe", r"C:\$(Arg0)\agentspads.exe"] {
            assert!(task_xml(USER, Path::new(path), Kind::Admin, true).is_err());
        }
        let xml = task_xml(
            USER,
            Path::new(r"C:\Program Files\AgentsPads\agentspads.exe"),
            Kind::Admin,
            true,
        )
        .unwrap();
        assert!(xml.contains("<Arguments>--elevated</Arguments>"));
        assert!(xml.contains("<Triggers/>"));
        assert!(!xml.contains("LogonTrigger"));
        assert!(!xml.contains("$("));
    }

    #[test]
    fn normal_login_startup_does_not_elevate_or_retarget_admin_entry() {
        let path = Path::new(r"C:\Program Files\AgentsPads\agentspads.exe");
        let normal = task_xml(USER, path, Kind::Autostart, false).unwrap();
        assert!(normal.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(normal.contains("<Arguments>--login-startup</Arguments>"));
        assert!(!normal.contains("--elevated"));
        let admin = task_xml(USER, path, Kind::Autostart, true).unwrap();
        assert!(admin.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(admin.contains("<Arguments>--elevated</Arguments>"));
        assert!(task_xml(USER, path, Kind::Admin, false).is_err());
    }

    #[test]
    fn windows_scheduler_validates_all_three_definitions_without_registering() {
        let _apartment = ComApartment::initialize().unwrap();
        let service: ITaskService =
            unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) }.unwrap();
        let empty = VARIANT::default();
        unsafe { service.Connect(&empty, &empty, &empty, &empty) }.unwrap();
        let sid = current_user_sid().unwrap();
        let path = Path::new(r"C:\Program Files\AgentsPads\agentspads.exe");
        for (kind, elevated) in [
            (Kind::Admin, true),
            (Kind::Autostart, true),
            (Kind::Autostart, false),
        ] {
            let definition = unsafe { service.NewTask(0) }.unwrap();
            let xml = task_xml(&sid, path, kind, elevated).unwrap();
            unsafe { definition.SetXmlText(&BSTR::from(xml.as_str())) }.unwrap();
            let mut level = Default::default();
            unsafe { definition.Principal().unwrap().RunLevel(&mut level) }.unwrap();
            assert_eq!(
                level,
                if elevated {
                    TASK_RUNLEVEL_HIGHEST
                } else {
                    TASK_RUNLEVEL_LUA
                }
            );
            validate_definition(&definition, &sid, kind, elevated, path).unwrap();
        }
    }

    #[test]
    fn windows_scheduler_rejects_changed_accounts_actions_and_policies() {
        let _apartment = ComApartment::initialize().unwrap();
        let service: ITaskService =
            unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) }.unwrap();
        let empty = VARIANT::default();
        unsafe { service.Connect(&empty, &empty, &empty, &empty) }.unwrap();
        let sid = current_user_sid().unwrap();
        let path = Path::new(r"C:\Program Files\AgentsPads\agentspads.exe");
        let xml = task_xml(&sid, path, Kind::Admin, true).unwrap();
        for (before, after, field) in [
            ("HighestAvailable", "LeastPrivilege", "运行权限"),
            ("--elevated", "--launch-admin", "启动参数"),
            (
                "<Command>C:\\Program Files\\AgentsPads\\agentspads.exe</Command>",
                "<Command>C:\\Windows\\System32\\cmd.exe</Command>",
                "程序路径",
            ),
            (
                "<WorkingDirectory>C:\\Program Files\\AgentsPads</WorkingDirectory>",
                "<WorkingDirectory>C:\\Windows</WorkingDirectory>",
                "工作目录",
            ),
            (
                "<AllowStartOnDemand>true",
                "<AllowStartOnDemand>false",
                "手动启动",
            ),
            (
                "<AllowHardTerminate>true",
                "<AllowHardTerminate>false",
                "停止方式",
            ),
            (
                "<StopIfGoingOnBatteries>false",
                "<StopIfGoingOnBatteries>true",
                "电池停止设置",
            ),
            ("PT0S", "PT72H", "运行时限"),
            (
                "<MultipleInstancesPolicy>Parallel",
                "<MultipleInstancesPolicy>IgnoreNew",
                "并发方式",
            ),
        ] {
            assert!(xml.contains(before));
            let changed = xml.replace(before, after);
            let definition = unsafe { service.NewTask(0) }.unwrap();
            unsafe { definition.SetXmlText(&BSTR::from(changed.as_str())) }.unwrap();
            let reason =
                validate_definition(&definition, &sid, Kind::Admin, true, path).unwrap_err();
            assert!(reason.contains(field), "{reason}");
        }
        let definition = unsafe { service.NewTask(0) }.unwrap();
        unsafe { definition.SetXmlText(&BSTR::from(xml.as_str())) }.unwrap();
        assert!(
            validate_definition(&definition, USER, Kind::Admin, true, path)
                .unwrap_err()
                .contains("运行账户")
        );

        let login_xml = task_xml(&sid, path, Kind::Autostart, true).unwrap();
        unsafe { definition.SetXmlText(&BSTR::from(login_xml.as_str())) }.unwrap();
        let trigger: ILogonTrigger = unsafe { definition.Triggers().unwrap().get_Item(1) }
            .unwrap()
            .cast()
            .unwrap();
        unsafe { trigger.SetUserId(&BSTR::from(SYSTEM_SID)) }.unwrap();
        assert!(
            validate_definition(&definition, &sid, Kind::Autostart, true, path)
                .unwrap_err()
                .contains("登录触发账户")
        );
    }

    #[test]
    fn account_resolution_preserves_identity_and_rejects_invalid_names() {
        let sid = current_user_sid().unwrap();
        assert_eq!(account_sid(&sid), Some(sid));
        assert_eq!(account_sid(SYSTEM_SID).as_deref(), Some(SYSTEM_SID));
        for invalid in [
            "",
            "S-1-invalid",
            "account\0suffix",
            "AgentsPads-No-Such-Account-109c5fdd",
        ] {
            assert_eq!(account_sid(invalid), None);
        }
    }

    #[test]
    #[ignore = "requires an administrator token; creates and removes disabled temporary tasks"]
    fn registered_tasks_preserve_identity_policy_and_protected_permissions() {
        let token = current_token().unwrap();
        assert!(token_elevated(HANDLE(token.as_raw_handle())).unwrap());
        let sid = current_user_sid().unwrap();
        let mut directory = vec![0u16; 32768];
        let count = unsafe { GetWindowsDirectoryW(Some(&mut directory)) } as usize;
        assert!(count > 0 && count < directory.len());
        let path = PathBuf::from(OsString::from_wide(&directory[..count]))
            .join("System32")
            .join("cmd.exe");
        for (kind, elevated) in [
            (Kind::Admin, true),
            (Kind::Autostart, true),
            (Kind::Autostart, false),
        ] {
            let name = format!("AgentsPads.Validation.{}", uuid::Uuid::new_v4());
            let xml = task_xml(&sid, &path, kind, elevated).unwrap().replace(
                "<AllowStartOnDemand>true</AllowStartOnDemand><Enabled>true</Enabled>",
                "<AllowStartOnDemand>true</AllowStartOnDemand><Enabled>false</Enabled>",
            );
            with_folder(|folder| {
                let registered = unsafe {
                    folder.RegisterTask(
                        &BSTR::from(name.as_str()),
                        &BSTR::from(xml.as_str()),
                        TASK_CREATE_OR_UPDATE.0
                            | TASK_DONT_ADD_PRINCIPAL_ACE.0
                            | TASK_IGNORE_REGISTRATION_TRIGGERS.0,
                        &VARIANT::from(sid.as_str()),
                        &VARIANT::default(),
                        TASK_LOGON_INTERACTIVE_TOKEN,
                        &VARIANT::from(task_sddl(&sid).as_str()),
                    )
                }
                .map_err(|e| error("temporary task registration", e))?;
                let result = (|| {
                    let enabled = unsafe { registered.Enabled() }
                        .map_err(|e| error("temporary task state", e))?;
                    if enabled.as_bool() {
                        return Err("temporary test task must remain disabled".into());
                    }
                    let definition = unsafe { registered.Definition() }
                        .map_err(|e| error("temporary task definition", e))?;
                    validate_definition(&definition, &sid, kind, elevated, &path)?;
                    let descriptor = unsafe { registered.GetSecurityDescriptor(OWNER_AND_DACL) }
                        .map_err(|e| error("temporary task permissions", e))?;
                    validate_security_descriptor(&descriptor.to_string(), &sid)
                })();
                unsafe { folder.DeleteTask(&BSTR::from(name.as_str()), 0) }
                    .map_err(|e| error("temporary task cleanup", e))?;
                result
            })
            .unwrap();
        }
    }
}
