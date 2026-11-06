$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'validate-install-root.ps1') -Mode Functions
$script:Checks = 0

function Assert-Equal($Actual, $Expected) {
    if ($Actual -ne $Expected) { throw "Expected '$Expected', got '$Actual'." }
    $script:Checks++
}

function Assert-Rejected([scriptblock]$Action) {
    $rejected = $false
    try { & $Action | Out-Null } catch { $rejected = $true }
    if (-not $rejected) { throw 'An unsafe input was accepted.' }
    $script:Checks++
}

function Test-Descriptor([string]$Sddl, [string]$Role) {
    Assert-SafeDescriptor ([Security.AccessControl.RawSecurityDescriptor]::new($Sddl)) $Role
}

$user = 'S-1-5-21-100-200-300-1001'
Assert-Equal (Get-NormalizedInstallDirectory 'F:\Apps\AgentsPads\') 'F:\Apps\AgentsPads'
foreach ($path in @('F:\', '\\server\share\AgentsPads', 'F:AgentsPads', 'F:\Apps\..\AgentsPads', 'F:\Apps.\AgentsPads', 'F:\Apps \AgentsPads', 'F:\%TEMP%\AgentsPads', 'F:\$(Arg0)\AgentsPads', 'F:\Apps\file:stream', 'F:\Apps\\AgentsPads', 'F:\con\AgentsPads', 'F:\Com1.txt\AgentsPads', 'F:\nul.zip')) {
    Assert-Rejected { Get-NormalizedInstallDirectory $path }
}
foreach ($role in @('VolumeRoot', 'Ancestor', 'Application', 'Registry')) {
    $read = if ($role -eq 'Registry') { 'KR' } else { 'FRFX' }
    Test-Descriptor "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;$read;;;BU)" $role
    $script:Checks++
    Assert-Rejected { Test-Descriptor "O:$($user)G:BAD:P(A;;FA;;;BA)(A;;FRFX;;;BU)" $role }
    foreach ($right in @('GA', 'GW', 'WD', 'WO', '0x40', '0x1000000', '0x2000000')) {
        if ($role -eq 'Registry' -and $right -eq '0x40') { continue }
        Assert-Rejected { Test-Descriptor "O:BAG:BAD:P(A;;FA;;;BA)(A;;$right;;;$user)" $role }
    }
}
Test-Descriptor 'O:SYG:SYD:PAI(A;OICI;0x1301bf;;;AU)(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FRFX;;;BU)' 'VolumeRoot'
$script:Checks++
Assert-Rejected { Test-Descriptor 'O:SYG:SYD:PAI(A;OICI;0x1301bf;;;AU)(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)' 'Ancestor' }
Test-Descriptor 'O:BAG:BAD:P(A;;FA;;;BA)(A;;0x6;;;AU)(A;OICIIO;GA;;;CO)' 'Ancestor'
$script:Checks++
Assert-Rejected { Test-Descriptor 'O:BAG:BAD:P(A;;FA;;;BA)(A;;0x6;;;AU)' 'Application' }
Test-Descriptor 'O:BAG:BAD:P(A;;KA;;;SY)(A;;KA;;;BA)(A;;KR;;;BU)' 'Registry'
$script:Checks++
foreach ($right in @('0x2', '0x4', '0x20', 'SD')) {
    Assert-Rejected { Test-Descriptor "O:BAG:BAD:P(A;;KA;;;BA)(A;;$right;;;$user)" 'Registry' }
}

$drives = foreach ($letter in @('G', 'F', 'C', 'D', 'E')) {
    [pscustomobject]@{ Name = "${letter}:\"; IsReady = $true; DriveType = [IO.DriveType]::Fixed; DriveFormat = 'NTFS'; AvailableFreeSpace = 1GB }
}
Assert-Equal (Select-InstallCandidate $drives 'C:\' { param($path) if ($path -eq 'D:\AgentsPads') { throw 'Unsafe drive' } }) 'E:\AgentsPads'
($drives | Where-Object Name -eq 'D:\').DriveType = [IO.DriveType]::Removable
($drives | Where-Object Name -eq 'E:\').AvailableFreeSpace = 1MB
($drives | Where-Object Name -eq 'F:\').DriveFormat = 'exFAT'
Assert-Equal (Select-InstallCandidate $drives 'C:\' { }) 'G:\AgentsPads'
Assert-Equal (Select-InstallCandidate $drives 'G:\' { }) $null
($drives | Where-Object Name -eq 'F:\').DriveFormat = 'NTFS'
($drives | Where-Object Name -eq 'F:\').IsReady = $false
Assert-Equal (Select-InstallCandidate $drives 'G:\' { }) $null
foreach ($bus in @('NVMe', 'SATA', 'SCSI', 'RAID', 'Virtual')) { Assert-Equal (Test-AutomaticBusType $bus) $true }
foreach ($bus in @('USB', 'SD', 'MMC', 'iSCSI', 'Unknown', '')) { Assert-Equal (Test-AutomaticBusType $bus) $false }

$existing = Get-ExistingInstallDirectory
if ($existing) {
    Assert-Equal (Get-DefaultInstallDirectory) $existing
    Assert-InstallDirectory $existing | Out-Null
    $script:Checks++
    Assert-Rejected { Assert-InstallDirectory 'C:\AgentsPads-Other-Location' }
}
"Passed $script:Checks installation location checks. No files, registry keys, or tasks were changed."
