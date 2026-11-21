[CmdletBinding()]
param(
    [string]$Version = "1.8.4",
    [switch]$Analyze
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$project = Join-Path $repo "src\BetterTrafficMonitorAiUsage\BetterTrafficMonitorAiUsage.vcxproj"
$build = Join-Path $env:TEMP "better-trafficmonitor-ai-usage-build\release-x64"
$bin = Join-Path $build "bin"
$obj = Join-Path $build "obj"
$dist = Join-Path $repo "dist"
$stage = Join-Path $build "package"
New-Item -ItemType Directory -Path $build -Force | Out-Null
$evidence = Join-Path $repo "artifacts\native-build"
New-Item -ItemType Directory -Path $evidence -Force | Out-Null

$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargo) {
    $cargo = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
}
if (-not (Test-Path -LiteralPath $cargo)) { throw "cargo was not found" }

$manifest = Join-Path $repo "Cargo.toml"
function Invoke-CargoChecked([string[]]$CargoArguments, [string]$Label) {
    Write-Host "Running cargo $Label"
    # Direct invocation keeps arguments with spaces (for example a user
    # profile path) properly quoted; Start-Process -ArgumentList splits them.
    & $cargo @CargoArguments
    if ($LASTEXITCODE -ne 0) { throw "cargo $Label failed" }
}

Invoke-CargoChecked @("test", "--locked", "--release", "--workspace", "--all-targets", "--manifest-path", $manifest) "test"
Invoke-CargoChecked @("build", "--locked", "--release", "-p", "usage_core", "--manifest-path", $manifest) "build"

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vswhereOutput = Join-Path $build "vswhere-msbuild.txt"
$vswhereProcess = Start-Process -FilePath $vswhere `
    -ArgumentList @("-latest", "-products", "*", "-requires", "Microsoft.Component.MSBuild", "-find", "MSBuild\**\Bin\MSBuild.exe") `
    -NoNewWindow -Wait -PassThru -RedirectStandardOutput $vswhereOutput
if ($vswhereProcess.ExitCode -ne 0) { throw "vswhere failed" }
$msbuild = Get-Content -LiteralPath $vswhereOutput | Select-Object -First 1
if (-not $msbuild) { throw "MSBuild was not found" }

$msbuildResponse = Join-Path $build "msbuild.rsp"
# Quote every path: response-file arguments are split on spaces, and the
# repository can live under a profile path that contains one. Trailing
# backslashes are doubled so the closing quote is not escaped.
@(
    "`"$project`""
    "-m"
    "-t:Rebuild"
    "-p:Configuration=Release"
    "-p:Platform=x64"
    "-p:RunCodeAnalysis=$($Analyze.IsPresent.ToString().ToLowerInvariant())"
    "-p:IntDir=`"$obj\\`""
    "-p:OutDir=`"$bin\\`""
    "-fl"
    "-flp:LogFile=`"$(Join-Path $evidence 'msbuild.log')`";Verbosity=normal"
) | Set-Content -LiteralPath $msbuildResponse -Encoding ASCII
Write-Host "Building the x64 TrafficMonitor DLL"
& $msbuild "@$msbuildResponse"
if ($LASTEXITCODE -ne 0) { throw "MSBuild failed" }

Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path (Join-Path $stage "plugins") -Force | Out-Null
New-Item -ItemType Directory -Path $dist -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $bin "BetterTrafficMonitorAiUsage.dll") -Destination (Join-Path $stage "plugins\BetterTrafficMonitorAiUsage.dll")
$dll = Join-Path $stage "plugins\BetterTrafficMonitorAiUsage.dll"
$metadata = (Get-Item -LiteralPath $dll).VersionInfo
if ($metadata.FileVersion -ne $Version -or $metadata.ProductVersion -ne $Version) {
    throw "DLL metadata does not match package version $Version (file=$($metadata.FileVersion), product=$($metadata.ProductVersion))"
}
$manifestVersion = (Select-String -LiteralPath (Join-Path $repo "crates\usage_core\Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
if ($manifestVersion -ne $Version) { throw "Rust manifest and package version differ" }

$zip = Join-Path $dist "BetterTrafficMonitorAiUsage_v${Version}_x64.zip"
Remove-Item -LiteralPath $zip -Force -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path $stage "plugins") -DestinationPath $zip -CompressionLevel Optimal
$hash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  $(Split-Path -Leaf $zip)" | Set-Content -LiteralPath "$zip.sha256" -Encoding ASCII
$sourceSha = (& git -C $repo rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw "Cannot resolve source commit for build evidence" }
@{
    sourceSha = $sourceSha
    version = $Version
    dllSha256 = (Get-FileHash -LiteralPath $dll -Algorithm SHA256).Hash.ToLowerInvariant()
    zipSha256 = $hash
    nativeAnalysisRequested = $Analyze.IsPresent
    nativeCompilerLog = "msbuild.log"
} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $evidence "build.json") -Encoding utf8

Write-Host "Created $zip"
Write-Host "SHA256 $hash"
