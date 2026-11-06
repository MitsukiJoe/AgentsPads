#![cfg(windows)]

use std::path::{Path, PathBuf};

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
};
use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumeInformationW, QueryDosDeviceW};
use windows::Win32::System::Registry::{
    RegCloseKey, RegGetKeySecurity, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE,
    KEY_READ, KEY_WOW64_64KEY, RRF_NOEXPAND, RRF_RT_REG_SZ, RRF_ZEROONFAILURE,
};

const REGISTRATION_KEY: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\AgentsPads.Managed.Stable_is1";

struct RegistryKey(HKEY);

impl Drop for RegistryKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

pub fn registered_directory() -> Result<PathBuf, String> {
    let mut raw = HKEY::default();
    unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(REGISTRATION_KEY),
            None,
            KEY_READ | KEY_WOW64_64KEY,
            &mut raw,
        )
    }
    .ok()
    .map_err(|_| "找不到正式安装位置，请使用安装包安装或修复")?;
    let key = RegistryKey(raw);
    let flags = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let mut size = 0;
    let result = unsafe { RegGetKeySecurity(key.0, flags, None, &mut size) };
    if result != ERROR_INSUFFICIENT_BUFFER || size == 0 || size > 65_536 {
        return Err("无法验证安装位置记录的权限，请使用安装包修复".into());
    }
    let mut descriptor = vec![0u32; (size as usize).div_ceil(4)];
    unsafe {
        RegGetKeySecurity(
            key.0,
            flags,
            Some(PSECURITY_DESCRIPTOR(descriptor.as_mut_ptr().cast())),
            &mut size,
        )
    }
    .ok()
    .map_err(|_| "无法读取安装位置记录的权限")?;
    crate::managed_tasks::validate_registry_descriptor(PSECURITY_DESCRIPTOR(
        descriptor.as_mut_ptr().cast(),
    ))
    .map_err(|_| "安装位置记录的权限不安全，请使用安装包修复")?;

    let mut buffer = [0u16; 1024];
    let mut bytes = std::mem::size_of_val(&buffer) as u32;
    unsafe {
        RegGetValueW(
            key.0,
            None,
            w!("InstallLocation"),
            RRF_NOEXPAND | RRF_RT_REG_SZ | RRF_ZEROONFAILURE,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    }
    .ok()
    .map_err(|_| "安装位置记录无效，请使用安装包修复")?;
    let units = bytes as usize / 2;
    if !bytes.is_multiple_of(2) || units < 2 || units > buffer.len() || buffer[units - 1] != 0 {
        return Err("安装位置记录无效，请使用安装包修复".into());
    }
    let text = String::from_utf16(&buffer[..units - 1]).map_err(|_| "安装路径编码无效")?;
    parse_directory(&text)
}

fn parse_directory(text: &str) -> Result<PathBuf, String> {
    let text = text.trim_end_matches('\\');
    let bytes = text.as_bytes();
    if bytes.len() < 4
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || bytes[2] != b'\\'
        || text.contains(['/', '"', '%', '\0', '\n', '\r'])
        || text.contains("$(")
        || text[2..].contains(':')
        || text[3..].split('\\').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part.ends_with(['.', ' '])
                || part.contains(['<', '>', '|', '?', '*'])
                || reserved_component(part)
        })
    {
        return Err("安装路径必须是本地磁盘上的独立文件夹，请使用安装包修复".into());
    }
    Ok(PathBuf::from(text))
}

fn reserved_component(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

pub fn validate_parent_directories(directory: &Path) -> Result<(), String> {
    let volume = directory.ancestors().last().ok_or("安装磁盘无效")?;
    let wide = HSTRING::from(volume.as_os_str());
    if unsafe { GetDriveTypeW(&wide) } != 3 {
        return Err("安装磁盘不可用，请连接原本的本地磁盘后重试".into());
    }
    let letter = volume
        .to_str()
        .ok_or("安装磁盘无效")?
        .trim_end_matches('\\');
    let mut device = [0u16; 1024];
    let count = unsafe { QueryDosDeviceW(&HSTRING::from(letter), Some(&mut device)) };
    if count == 0 || count as usize > device.len() {
        return Err("无法验证安装磁盘的位置".into());
    }
    let length = device.iter().position(|v| *v == 0).unwrap_or(device.len());
    if !is_direct_volume(&String::from_utf16_lossy(&device[..length])) {
        return Err("安装位置不能使用替代盘符或映射目录，请选择本地磁盘".into());
    }
    let mut filesystem = [0u16; 32];
    unsafe { GetVolumeInformationW(&wide, None, None, None, None, Some(&mut filesystem)) }
        .map_err(|_| "无法读取安装磁盘")?;
    let length = filesystem
        .iter()
        .position(|v| *v == 0)
        .unwrap_or(filesystem.len());
    if !String::from_utf16_lossy(&filesystem[..length]).eq_ignore_ascii_case("NTFS") {
        return Err("正式程序需要安装在支持权限保护的 NTFS 磁盘".into());
    }
    for ancestor in directory.ancestors().skip(1) {
        crate::managed_tasks::validate_install_ancestor(ancestor, ancestor == volume)?;
    }
    Ok(())
}

fn is_direct_volume(device: &str) -> bool {
    device
        .strip_prefix(r"\Device\HarddiskVolume")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_registered_non_system_and_unicode_locations() {
        for path in [
            r"F:\AgentsPads",
            r"E:\应用\AgentsPads",
            r"C:\Program Files\AgentsPads",
        ] {
            assert_eq!(
                parse_directory(&format!("{path}\\")).unwrap(),
                PathBuf::from(path)
            );
        }
    }

    #[test]
    fn rejects_ambiguous_redirected_or_expandable_locations() {
        for path in [
            r"F:\",
            r"F:AgentsPads",
            r"\AgentsPads",
            r"\\server\app",
            r"\\?\C:\app",
            r"C:\app\..\other",
            r"C:\app\.\other",
            r"C:\\app",
            r"C:\app.",
            r"C:\app ",
            r"C:\app:stream",
            r"C:\%TEMP%\app",
            r"C:\$(Arg0)\app",
            r"C:/app",
            r"C:\NUL\app",
            r"C:\con.txt\app",
            r"C:\COM1\app",
            "C:\\app\0suffix",
        ] {
            assert!(parse_directory(path).is_err(), "{path:?}");
        }
    }

    #[test]
    fn substituted_directories_cannot_receive_volume_root_permissions() {
        assert!(is_direct_volume(r"\Device\HarddiskVolume12"));
        for value in [
            r"\??\D:\user-directory",
            r"\Device\HarddiskVolume12\user-directory",
            r"\Device\Mup\server\share",
            r"\Device\HarddiskVolume",
            "",
        ] {
            assert!(!is_direct_volume(value), "{value}");
        }
    }
}
