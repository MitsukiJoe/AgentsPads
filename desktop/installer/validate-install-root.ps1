[CmdletBinding()]
param(
    [ValidateSet('Select', 'Validate', 'Prepare', 'ProtectRegistration', 'Functions')]
    [string]$Mode = 'Validate',
    [string]$InstallDirectory,
    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'
$script:InstallKey = 'SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\AgentsPads.Managed.Stable_is1'
$script:TrustedOwners = @(
    'S-1-5-18',
    'S-1-5-32-544',
    'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464'
)
$script:MinimumFreeBytes = 256MB

function Get-NormalizedInstallDirectory([string]$Path) {
    if ($Path -notmatch '^[a-zA-Z]:\\[^\\]' -or $Path.Contains('/') -or $Path.Contains('%') -or $Path.Contains('$(')) {
        throw 'Choose a local drive folder, not a network, relative, or substituted path.'
    }
    foreach ($component in $Path.Substring(3).TrimEnd('\').Split('\')) {
        if (-not $component -or $component -in @('.', '..') -or $component -match '[<>:"|?*\x00-\x1f]' -or
            $component -match '[. ]$' -or $component -match '^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(\.|$)') {
            throw 'The installation folder name is not supported.'
        }
    }
    $full = [IO.Path]::GetFullPath($Path).TrimEnd('\')
    if ($full.Length -gt 180) { throw 'Choose a shorter installation folder path.' }
    return $full
}

function Get-ExistingInstallDirectory {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey($script:InstallKey)
        if (-not $key) { return $null }
        try {
            if ($key.GetValueKind('InstallLocation') -ne [Microsoft.Win32.RegistryValueKind]::String) { throw 'The existing installation path is not a literal folder.' }
            $location = $key.GetValue('InstallLocation', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            if (-not ($location -is [string]) -or -not $location) { throw 'The existing installation record is incomplete. Repair or uninstall it first.' }
            return Get-NormalizedInstallDirectory $location
        } finally { $key.Dispose() }
    } finally { $base.Dispose() }
}

function Assert-SafeDescriptor([Security.AccessControl.RawSecurityDescriptor]$Descriptor, [string]$Role) {
    if ($Descriptor.Owner.Value -notin $script:TrustedOwners -or -not $Descriptor.DiscretionaryAcl) {
        throw 'The path is not owned and protected by Windows administrators.'
    }
    $mask = switch ($Role) {
        'VolumeRoot' { [uint32]0x530c0040 }
        'Ancestor' { [uint32]0x530d0150 }
        'Application' { [uint32]0x530d0156 }
        'Registry' { [uint32]0x530d0026 }
        default { throw 'Unknown path role.' }
    }
    foreach ($ace in $Descriptor.DiscretionaryAcl) {
        if (([int]$ace.AceFlags -band [int][Security.AccessControl.AceFlags]::InheritOnly) -ne 0) { continue }
        if ($ace -isnot [Security.AccessControl.CommonAce] -or $ace.IsCallback) { throw 'An unsupported access rule protects this path.' }
        if ($ace.AceQualifier -eq [Security.AccessControl.AceQualifier]::AccessDenied) { continue }
        if ($ace.AceQualifier -ne [Security.AccessControl.AceQualifier]::AccessAllowed) { throw 'An unsupported access rule protects this path.' }
        $rights = [uint32]([long]$ace.AccessMask -band 0xffffffffL)
        if (($rights -band $mask) -and $ace.SecurityIdentifier.Value -notin $script:TrustedOwners) {
            throw 'An ordinary process can replace or redirect this path.'
        }
    }
}

function Assert-SafePath([string]$Path, [string]$Role) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "A link or junction is not allowed: $Path" }
    if ($Role -ne 'Application' -and -not $item.PSIsContainer) { throw "A parent path is not a folder: $Path" }
    $acl = Microsoft.PowerShell.Security\Get-Acl -LiteralPath $Path
    $descriptor = [Security.AccessControl.RawSecurityDescriptor]::new($acl.GetSecurityDescriptorBinaryForm(), 0)
    try { Assert-SafeDescriptor $descriptor $Role } catch { throw "$($_.Exception.Message) Path: $Path" }
}

function Assert-SupportedDrive([string]$Directory) {
    Initialize-ProtectedDirectoryApi
    $mapping = [AgentsPads.InstallDirectoryNative]::DriveTarget($Directory.Substring(0, 2))
    if ($mapping -notmatch '^\\Device\\HarddiskVolume\d+$') { throw 'Substituted or redirected drive letters cannot be used.' }
    $drive = [IO.DriveInfo]::new([IO.Path]::GetPathRoot($Directory))
    if ($drive.DriveType -ne [IO.DriveType]::Fixed -or -not $drive.IsReady -or $drive.DriveFormat -ne 'NTFS') {
        throw 'Choose an available local NTFS fixed drive.'
    }
    if ($drive.AvailableFreeSpace -lt $script:MinimumFreeBytes) { throw 'The selected drive needs at least 256 MB of free space.' }
    return $drive
}

function Assert-InstallDirectory([string]$Directory, [bool]$CheckRegistered = $true) {
    $directory = Get-NormalizedInstallDirectory $Directory
    if ($CheckRegistered) {
        $registered = Get-ExistingInstallDirectory
        if ($registered -and -not $directory.Equals($registered, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'AgentsPads is already installed elsewhere. Uninstall it before choosing another location.'
        }
    }
    $drive = Assert-SupportedDrive $directory
    $current = [IO.DirectoryInfo]::new($directory).Parent
    while ($current) {
        if (Test-Path -LiteralPath $current.FullName) {
            $role = if ($current.FullName -eq $drive.RootDirectory.FullName) { 'VolumeRoot' } else { 'Ancestor' }
            Assert-SafePath $current.FullName $role
        }
        $current = $current.Parent
    }
    $paths = @($directory)
    $paths += @('agentspads.exe', 'agentspads.exe.installer-backup', 'managed-install.json', 'launch-mode-initialization.pending', 'operation.lock', 'updates', 'updates\transaction.json', 'updates\update-worker.exe') |
        ForEach-Object { Join-Path $directory $_ }
    foreach ($path in $paths) {
        if (Test-Path -LiteralPath $path) { Assert-SafePath $path 'Application' }
    }
    if (Test-Path -LiteralPath $directory) {
        if (-not (Get-Item -LiteralPath $directory -Force).PSIsContainer) { throw 'The installation path is not a folder.' }
        if (-not (Test-Path -LiteralPath (Join-Path $directory 'managed-install.json')) -and
            (Get-ChildItem -LiteralPath $directory -Force | Select-Object -First 1)) {
            throw 'The selected folder already contains unrelated files. Choose an empty folder.'
        }
    }
    return $directory
}

function Select-InstallCandidate($Drives, [string]$SystemRoot, [scriptblock]$ValidateCandidate) {
    foreach ($drive in ($Drives | Sort-Object Name)) {
        if ($drive.Name -eq 'C:\' -or $drive.Name -eq $SystemRoot -or $drive.DriveType -ne [IO.DriveType]::Fixed -or
            -not $drive.IsReady -or $drive.DriveFormat -ne 'NTFS' -or
            $drive.AvailableFreeSpace -lt $script:MinimumFreeBytes) { continue }
        $candidate = [IO.Path]::Combine($drive.Name, 'AgentsPads')
        try {
            & $ValidateCandidate $candidate | Out-Null
            return $candidate
        } catch { continue }
    }
    return $null
}

function Get-DefaultInstallDirectory {
    $existing = Get-ExistingInstallDirectory
    if ($existing) { return $existing }
    $systemRoot = [IO.Path]::GetPathRoot([Environment]::GetFolderPath([Environment+SpecialFolder]::Windows))
    $candidate = Select-InstallCandidate ([IO.DriveInfo]::GetDrives()) $systemRoot {
        param($path)
        Assert-AutomaticDrive $path
        Assert-InstallDirectory $path $false
    }
    if ($candidate) { return $candidate }
    return Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles)) 'AgentsPads'
}

function Assert-AutomaticDrive([string]$Directory) {
    $disks = @(Get-Partition -DriveLetter $Directory.Substring(0, 1) -ErrorAction Stop | Get-Disk -ErrorAction Stop)
    if ($disks.Count -ne 1 -or -not (Test-AutomaticBusType ([string]$disks[0].BusType))) {
        throw 'This drive cannot be confirmed as a permanent local installation drive.'
    }
}

function Test-AutomaticBusType([string]$BusType) {
    return $BusType -in @('SCSI', 'ATA', 'RAID', 'SAS', 'SATA', 'Virtual', 'File Backed Virtual', 'FileBackedVirtual', 'Storage Spaces', 'Spaces', 'NVMe')
}

function Initialize-ProtectedDirectoryApi {
    if ('AgentsPads.InstallDirectoryNative' -as [type]) { return }
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
namespace AgentsPads {
    public static class InstallDirectoryNative {
        [StructLayout(LayoutKind.Sequential)]
        private struct SecurityAttributes {
            public int Length;
            public IntPtr Descriptor;
            [MarshalAs(UnmanagedType.Bool)] public bool InheritHandle;
        }
        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool ConvertStringSecurityDescriptorToSecurityDescriptorW(string sddl, uint revision, out IntPtr descriptor, out uint size);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool CreateDirectoryW(string path, ref SecurityAttributes attributes);
        [DllImport("kernel32.dll")] private static extern IntPtr LocalFree(IntPtr pointer);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern uint QueryDosDeviceW(string device, StringBuilder target, int length);
        public static string DriveTarget(string drive) {
            var target = new StringBuilder(32768);
            if (QueryDosDeviceW(drive, target, target.Capacity) == 0) throw new Win32Exception(Marshal.GetLastWin32Error());
            return target.ToString();
        }
        public static void Create(string path) {
            IntPtr descriptor;
            uint size;
            if (!ConvertStringSecurityDescriptorToSecurityDescriptorW("O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FRFX;;;BU)", 1, out descriptor, out size))
                throw new Win32Exception(Marshal.GetLastWin32Error());
            try {
                var attributes = new SecurityAttributes { Length = Marshal.SizeOf(typeof(SecurityAttributes)), Descriptor = descriptor };
                if (!CreateDirectoryW(path, ref attributes)) throw new Win32Exception(Marshal.GetLastWin32Error());
            } finally { LocalFree(descriptor); }
        }
    }
}
'@
}

function New-ProtectedInstallDirectory([string]$Directory) {
    $legacyTasks = @(Get-ScheduledTask -TaskName 'AgentsPads Elevated', 'AgentsPads Elevated Autostart' -ErrorAction SilentlyContinue)
    if ($legacyTasks.Count -gt 0) { throw 'A legacy AgentsPads startup task still exists. Remove the old installation before installing another copy.' }
    $legacyRoot = Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles)) 'AgentsPads'
    if ((Test-Path -LiteralPath (Join-Path $legacyRoot 'run_as_admin.txt')) -or
        (Test-Path -LiteralPath (Join-Path $legacyRoot 'ordinary-exe-path.bin')) -or
        ((Test-Path -LiteralPath (Join-Path $legacyRoot 'agentspads.exe')) -and
            -not (Test-Path -LiteralPath (Join-Path $legacyRoot 'managed-install.json')))) {
        throw 'The legacy Program Files installation must be preserved or removed before continuing.'
    }
    $directory = Assert-InstallDirectory $Directory
    $missing = [Collections.Generic.Stack[string]]::new()
    $current = [IO.DirectoryInfo]::new($directory)
    while (-not (Test-Path -LiteralPath $current.FullName)) {
        $missing.Push($current.FullName)
        $current = $current.Parent
    }
    if ($missing.Count -gt 0) { Initialize-ProtectedDirectoryApi }
    while ($missing.Count -gt 0) {
        $path = $missing.Pop()
        Assert-InstallDirectory $directory | Out-Null
        [AgentsPads.InstallDirectoryNative]::Create($path)
        Assert-SafePath $path 'Application'
    }
    Assert-InstallDirectory $directory | Out-Null
}

function Protect-InstallationRegistration([string]$Directory) {
    $directory = Assert-InstallDirectory $Directory
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $rights = [Security.AccessControl.RegistryRights]::ReadKey -bor
            [Security.AccessControl.RegistryRights]::ChangePermissions -bor [Security.AccessControl.RegistryRights]::TakeOwnership
        $key = $base.OpenSubKey($script:InstallKey, [Microsoft.Win32.RegistryKeyPermissionCheck]::ReadWriteSubTree, $rights)
        if (-not $key) { throw 'The installer has not written its installation record.' }
        try {
            $location = Get-NormalizedInstallDirectory ($key.GetValue('InstallLocation', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames))
            if (-not $directory.Equals($location, [StringComparison]::OrdinalIgnoreCase)) { throw 'The installation record points to another folder.' }
            $security = [Security.AccessControl.RegistrySecurity]::new()
            $sections = [Security.AccessControl.AccessControlSections]::Access -bor [Security.AccessControl.AccessControlSections]::Owner
            $security.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;;KA;;;SY)(A;;KA;;;BA)(A;;KR;;;BU)', $sections)
            $key.SetAccessControl($security)
            $actual = $key.GetAccessControl($sections)
            if (-not $actual.AreAccessRulesProtected) { throw 'The installation record still inherits unsafe permissions.' }
            Assert-SafeDescriptor ([Security.AccessControl.RawSecurityDescriptor]::new($actual.GetSecurityDescriptorBinaryForm(), 0)) 'Registry'
            if ($key.GetValueKind('InstallLocation') -ne [Microsoft.Win32.RegistryValueKind]::String) { throw 'The protected installation record is not a literal folder.' }
            $protectedLocation = Get-NormalizedInstallDirectory ($key.GetValue('InstallLocation', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames))
            if (-not $directory.Equals($protectedLocation, [StringComparison]::OrdinalIgnoreCase)) { throw 'The installation location changed before its protection was applied.' }
        } finally { $key.Dispose() }
    } finally { $base.Dispose() }
}

function Write-LocationResult([string]$Text) {
    if ($OutputPath) { [IO.File]::WriteAllText($OutputPath, $Text, [Text.UTF8Encoding]::new($true)) }
    else { Write-Output $Text }
}

if ($Mode -eq 'Functions') { return }
try {
    switch ($Mode) {
        'Select' { Write-LocationResult (Get-DefaultInstallDirectory) }
        'Validate' { Assert-InstallDirectory $InstallDirectory | Out-Null }
        'Prepare' { New-ProtectedInstallDirectory $InstallDirectory }
        'ProtectRegistration' { Protect-InstallationRegistration $InstallDirectory }
    }
    exit 0
} catch {
    Write-LocationResult $_.Exception.Message
    Write-Error $_ -ErrorAction Continue
    exit 1
}
