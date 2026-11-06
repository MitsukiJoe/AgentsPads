[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version,
    [Parameter(Mandatory)]
    [string]$Executable,
    [string]$OutputDirectory = (Join-Path $PSScriptRoot 'dist'),
    [string]$Compiler
)

$ErrorActionPreference = 'Stop'
$executablePath = (Resolve-Path -LiteralPath $Executable).Path
if ([IO.Path]::GetExtension($executablePath) -ne '.exe') {
    throw 'Executable must be the Windows release EXE.'
}
$versionInfo = [Diagnostics.FileVersionInfo]::GetVersionInfo($executablePath)
if ($versionInfo.ProductName -ne 'AgentsPads' -or $versionInfo.ProductVersion -ne $Version) {
    throw "The EXE must be AgentsPads $Version; found '$($versionInfo.ProductName)' version '$($versionInfo.ProductVersion)'."
}
if (-not $Compiler) {
    $compilerCommand = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($compilerCommand) { $Compiler = $compilerCommand.Source }
}
if (-not $Compiler) {
    $candidateDirectories = @(
        [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles),
        [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFilesX86)
    ) | Select-Object -Unique
    foreach ($directory in $candidateDirectories) {
        foreach ($major in @(7, 6)) {
            $candidate = Join-Path $directory "Inno Setup $major\ISCC.exe"
            if (Test-Path -LiteralPath $candidate) { $Compiler = $candidate; break }
        }
        if ($Compiler) { break }
    }
}
if (-not $Compiler -or -not (Test-Path -LiteralPath $Compiler)) {
    throw 'Inno Setup 6.5 or later is required. Supply its ISCC.exe with -Compiler.'
}
$compilerPath = (Resolve-Path -LiteralPath $Compiler).Path
$compilerVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo($compilerPath)
if ($compilerVersion.ProductName -ne 'Inno Setup') {
    throw 'Compiler must be the Inno Setup command-line compiler. The script checks its engine version during compilation.'
}
$outputPath = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $outputPath -Force | Out-Null
$arguments = @(
    '/Qp',
    "/DAppVersion=$Version",
    "/DAppExecutable=$executablePath",
    "/DAppSha256=$((Get-FileHash -LiteralPath $executablePath -Algorithm SHA256).Hash.ToLowerInvariant())",
    "/O$outputPath",
    (Join-Path $PSScriptRoot 'agentspads.iss')
)
& $compilerPath @arguments
if ($LASTEXITCODE -ne 0) { throw "Installer compilation failed with exit code $LASTEXITCODE." }
$installerPath = Join-Path $outputPath 'agentspads-windows-x64-setup.exe'
if (-not (Test-Path -LiteralPath $installerPath)) { throw 'Compiler did not produce the expected installer.' }
Get-Item -LiteralPath $installerPath | Select-Object FullName, Length
