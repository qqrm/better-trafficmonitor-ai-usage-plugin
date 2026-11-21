[CmdletBinding()]
param([string]$Package)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
if (-not $Package) {
    $packages = @(Get-ChildItem -LiteralPath (Join-Path $repo 'dist') -Filter '*_x64.zip')
    if ($packages.Count -ne 1) { throw 'Specify exactly one built package' }
    $Package = $packages[0].FullName
}
$Package = (Resolve-Path -LiteralPath $Package).Path
$evidence = Join-Path $repo 'artifacts\native-build'
$buildEvidence = Get-Content -Raw -LiteralPath (Join-Path $evidence 'build.json') | ConvertFrom-Json
if ((Get-FileHash -LiteralPath $Package -Algorithm SHA256).Hash.ToLowerInvariant() -ne $buildEvidence.zipSha256) {
    throw 'The package differs from the analyzed build'
}
$scratch = Join-Path ([System.IO.Path]::GetTempPath()) ('ai-plugin-smoke-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $scratch | Out-Null
try {
    Expand-Archive -LiteralPath $Package -DestinationPath (Join-Path $scratch 'package')
    $dll = Join-Path $scratch 'package\plugins\BetterTrafficMonitorAiUsage.dll'
    $dllHash = (Get-FileHash -LiteralPath $dll -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($dllHash -ne $buildEvidence.dllSha256) { throw 'Packaged DLL differs from the analyzed DLL' }
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($LASTEXITCODE -ne 0 -or -not $installation) { throw 'Visual C++ tools not found' }
    $vcvars = Join-Path ($installation | Select-Object -First 1) 'VC\Auxiliary\Build\vcvars64.bat'
    $exe = Join-Path $scratch 'native_dll_smoke.exe'
    $source = Join-Path $repo 'tests\native_dll_smoke.cpp'
    $include = Join-Path $repo 'include'
    $command = Join-Path $scratch 'build.cmd'
    @(
        '@echo off'
        "call `"$vcvars`" >nul"
        'if errorlevel 1 exit /b %errorlevel%'
        "cl /nologo /std:c++20 /EHsc /W4 /WX /analyze /I`"$include`" `"$source`" /Fe:`"$exe`" user32.lib gdi32.lib"
        'exit /b %errorlevel%'
    ) | Set-Content -LiteralPath $command -Encoding utf8NoBOM
    Push-Location $scratch
    try {
        & $env:ComSpec /d /c $command
        if ($LASTEXITCODE -ne 0) { throw 'Native DLL smoke driver compilation failed' }
    } finally { Pop-Location }
    $config = Join-Path $scratch 'config'
    New-Item -ItemType Directory -Path $config | Out-Null
    $stdout = Join-Path $evidence 'dll-smoke.stdout.log'
    $stderr = Join-Path $evidence 'dll-smoke.stderr.log'
    $process = Start-Process -FilePath $exe -ArgumentList @("`"$dll`"", "`"$config`"", $buildEvidence.version) -PassThru -NoNewWindow -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    if (-not $process.WaitForExit(30000)) {
        $process.Kill()
        $process.WaitForExit()
        throw 'Production DLL smoke exceeded its 30 second watchdog'
    }
    $process.WaitForExit()
    Get-Content -LiteralPath $stdout
    Get-Content -LiteralPath $stderr
    if ($process.ExitCode -ne 0) { throw "Production DLL smoke failed: $($process.ExitCode)" }
    @{
        sourceSha = $buildEvidence.sourceSha
        dllSha256 = $dllHash
        zipSha256 = $buildEvidence.zipSha256
        status = 'passed'
        actualTrafficMonitor = $false
        externalProviderPolling = $false
        checks = @('factory', 'API', 'version', 'index bounds', 'provider IDs', 'initialization', 'cached text', 'GDI drawing', 'visibility configuration')
    } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $evidence 'dll-smoke.json') -Encoding utf8
} finally {
    Remove-Item -LiteralPath $scratch -Recurse -Force
}
