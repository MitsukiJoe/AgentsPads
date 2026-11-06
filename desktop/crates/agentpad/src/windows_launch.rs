use super::*;

#[cfg(windows)]
pub(super) fn prepare_admin_identity_at(
    dir: &std::path::Path,
) -> std::io::Result<std::path::PathBuf> {
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

    crate::managed_tasks::protect_secret_acl(dir).map_err(std::io::Error::other)?;

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

    crate::managed_tasks::protect_secret_acl(&file).map_err(std::io::Error::other)?;
    Ok(file)
}

#[cfg(windows)]
pub(super) fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

#[cfg(windows)]
pub(super) fn protected_target() -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let dir = crate::managed_install::registered_directory()?;
    if !no_reparse_ancestors(&dir) {
        return Err("安装目录已被重定向，请使用安装包修复".into());
    }
    let exe = dir.join("agentspads.exe");
    Ok((dir, exe))
}

#[cfg(windows)]
pub(super) fn no_reparse_ancestors(path: &std::path::Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    path.ancestors()
        .all(|part| match std::fs::symlink_metadata(part) {
            Ok(meta) => meta.file_attributes() & 0x400 == 0,
            Err(err) => err.kind() == std::io::ErrorKind::NotFound,
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(windows)]
pub(super) enum LaunchError {
    Cancelled,
    Failed,
}

#[cfg(windows)]
pub(super) fn shell_exec(
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
pub(super) fn shell_exec_wait(file: &std::ffi::OsStr, params: &str) -> Result<(), &'static str> {
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
pub(super) fn process_creation_time(pid: u32) -> Result<u64, String> {
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
struct SuspendedChild {
    process: std::os::windows::io::OwnedHandle,
    thread: std::os::windows::io::OwnedHandle,
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
fn create_suspended_medium_child(
    executable: &std::path::Path,
    args: &[String],
) -> Result<SuspendedChild, String> {
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
    command_line.pop();
    for arg in args {
        command_line.push(0x20);
        let mut quoted = quote_windows_wide(std::ffi::OsStr::new(arg));
        quoted.pop();
        command_line.extend(quoted);
    }
    command_line.push(0);
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

#[cfg(windows)]
pub(super) fn process_facts(
    pid: u32,
    hwnd: isize,
) -> Option<(ProcessFacts, bool, std::path::PathBuf)> {
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
                ProcessFacts {
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
pub(super) fn owns_listener(pid: u32) -> bool {
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
pub(super) fn window_belongs_to(raw: isize, pid: u32) -> bool {
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

pub(super) fn launch_medium(
    exe: &std::path::Path,
    args: &[String],
) -> Result<std::os::windows::io::OwnedHandle, String> {
    let child = create_suspended_medium_child(exe, args)?.resume()?;
    child.process.try_clone().map_err(|e| e.to_string())
}
