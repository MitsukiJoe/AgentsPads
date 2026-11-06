[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string[]]$Path
)

$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrEmpty($env:WINDOWS_CERTIFICATE_PFX_BASE64) -or
    [string]::IsNullOrEmpty($env:WINDOWS_CERTIFICATE_PASSWORD)) {
    Write-Host 'Windows signing secrets not configured; artifacts remain unsigned.'
    return
}
$artifactPaths = $Path | ForEach-Object { (Resolve-Path -LiteralPath $_).Path }
$certificatePath = Join-Path ([IO.Path]::GetTempPath()) ("agentpad-signing-" + [Guid]::NewGuid().ToString('N') + '.pfx')
try {
    [IO.File]::WriteAllBytes($certificatePath, [Convert]::FromBase64String($env:WINDOWS_CERTIFICATE_PFX_BASE64))
    $programFilesX86 = [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFilesX86)
    $kitsRoot = Join-Path $programFilesX86 'Windows Kits\10\bin'
    $signtool = Get-ChildItem "$kitsRoot\*\x64\signtool.exe" |
        Sort-Object FullName -Descending | Select-Object -First 1
    if (-not $signtool) { throw 'signtool.exe was not found.' }
    foreach ($artifact in $artifactPaths) {
        & $signtool.FullName sign /fd SHA256 /td SHA256 /tr http://timestamp.digicert.com /f $certificatePath /p $env:WINDOWS_CERTIFICATE_PASSWORD $artifact
        if ($LASTEXITCODE -ne 0) { throw "Signing failed with exit code $LASTEXITCODE." }
        & $signtool.FullName verify /pa /v $artifact
        if ($LASTEXITCODE -ne 0) { throw "Signature verification failed with exit code $LASTEXITCODE." }
    }
} finally {
    Remove-Item -LiteralPath $certificatePath -Force -ErrorAction SilentlyContinue
}
