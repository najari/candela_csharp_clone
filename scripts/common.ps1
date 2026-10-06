# ---------------------------------------------------------------------------
# DiagAuthor 공통 환경 준비 스크립트.
#
# 이 머신의 제약 두 가지.
#   1) .NET SDK 와 Rust 도구체인이 PATH 에 없다.
#   2) Windows SDK 가 시스템에 설치돼 있지 않다.
#      C:\Program Files (x86)\Windows Kits\10 에 레지스트리 키만 있고 Include/Lib 가 없다.
#      그래서 vcvars64.bat 을 불러도 INCLUDE 에 SDK 헤더가 안 들어간다.
#      번들 SQLite(libsqlite3-sys)가 sqlite3.c 를 빌드할 때 windows.h 를 못 찾아
#      cl.exe 가 실패하고, cargo build/cargo test 까지 깨진다.
#
# 해결: 저장소 .tools 에 벤더링된 Windows SDK 를 둔다. bootstrap.ps1 이 내려받고,
#      이 스크립트가 .tools 를 먼저 보고, 없을 때만 시스템 Kits 를 본다.
#
# 주의: .cmd 파일은 OEM 코드페이지로 읽히므로 한글을 넣으면 파서가 깨진다.
#       환경 준비는 PowerShell 로만 하고, .cmd 는 ASCII 래퍼로 둔다.
# ---------------------------------------------------------------------------

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-VsInstallPath {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path $vswhere) {
        $path = & $vswhere -latest -products * `
            -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
            -property installationPath
        if ($path) { return ($path | Select-Object -First 1) }
    }
    $fallback = 'C:\Program Files\Microsoft Visual Studio\2022\Community'
    if (Test-Path (Join-Path $fallback 'VC\Tools\MSVC')) { return $fallback }
    return $null
}

function Resolve-WindowsSdk {
    <#
      .SYNOPSIS
        Windows SDK 헤더/라이브러리 루트를 찾는다. 저장소 .tools 우선, 없으면 시스템 Kits.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$RepoRoot)

    $attempts = @(
        (Join-Path $RepoRoot '.tools\microsoft.windows.sdk.cpp\c\Include'),
        "${env:ProgramFiles(x86)}\Windows Kits\10\Include"
    )

    foreach ($includeRoot in $attempts) {
        if (-not (Test-Path $includeRoot)) { continue }

        $version = Get-ChildItem $includeRoot -Directory -ErrorAction SilentlyContinue |
                   Sort-Object Name -Descending | Select-Object -First 1
        if (-not $version) { continue }

        # 헤더 루트를 알았으므로 라이브러리 루트를 짝짓는다.
        $libRoot = if ($includeRoot -like '*\.tools\*') {
            Join-Path $RepoRoot '.tools\microsoft.windows.sdk.cpp.x64\c'
        } else {
            "${env:ProgramFiles(x86)}\Windows Kits\10\Lib"
        }
        if (-not (Test-Path $libRoot)) { continue }

        # 시스템 Kits 는 Lib/<version>/<flavor>, .tools 는 Lib 바로 아래 flavor 다.
        $versioned = Join-Path $libRoot $version.Name
        if (Test-Path $versioned) { $libRoot = $versioned }
        if (-not (Test-Path (Join-Path $libRoot 'um\x64'))) { continue }

        return [pscustomobject]@{
            Source     = if ($includeRoot -like '*\.tools\*') { 'repo .tools' } else { 'system Kits' }
            Version    = $version.Name
            IncludeDir = $version.FullName
            LibDir     = $libRoot
        }
    }
    return $null
}

function Initialize-DiagAuthorEnvironment {
    <#
      .SYNOPSIS
        MSVC 링커 + Windows SDK + cargo + dotnet 을 현재 프로세스에 준비한다.
      .OUTPUTS
        각 도구 경로와 SDK 출처를 담은 PSCustomObject.
    #>
    [CmdletBinding()]
    param([string]$RepoRoot = (Split-Path -Parent $PSScriptRoot))

    # --- 1. MSVC 툴체인 -------------------------------------------------------
    $vsRoot = Get-VsInstallPath
    if (-not $vsRoot) { throw 'MSVC 빌드 도구(Visual Studio 2022 C++ workload)를 찾지 못했다.' }

    $msvcRoot = Join-Path $vsRoot 'VC\Tools\MSVC'
    $msvc = Get-ChildItem $msvcRoot -Directory -ErrorAction SilentlyContinue |
            Sort-Object Name -Descending | Select-Object -First 1
    if (-not $msvc) { throw "MSVC 툴체인 디렉터리가 없다: $msvcRoot" }

    $msvcBin = Join-Path $msvc.FullName 'bin\Hostx64\x64'
    if (-not (Test-Path (Join-Path $msvcBin 'link.exe'))) { throw "link.exe 를 찾지 못했다: $msvcBin" }
    $env:PATH = "$msvcBin;$env:PATH"
    $env:LIB = "$(Join-Path $msvc.FullName 'lib\x64');$env:LIB"
    $env:INCLUDE = "$(Join-Path $msvc.FullName 'include');$env:INCLUDE"

    # --- 2. Windows SDK -------------------------------------------------------
    $sdk = Resolve-WindowsSdk -RepoRoot $RepoRoot
    if (-not $sdk) {
        throw @"
Windows SDK 를 찾지 못했다. 번들 SQLite 빌드가 cl.exe 단계에서 실패한다.
  해결: scripts\bootstrap.ps1 을 실행해 저장소 .tools 에 SDK를 받아 둔다.
  탐색한 위치:
    $RepoRoot\.tools\microsoft.windows.sdk.cpp\c\Include
    ${env:ProgramFiles(x86)}\Windows Kits\10\Include
"@
    }
    foreach ($flavor in @('ucrt', 'shared', 'um', 'winrt', 'cppwinrt')) {
        $dir = Join-Path $sdk.IncludeDir $flavor
        if (Test-Path $dir) { $env:INCLUDE = "$dir;$env:INCLUDE" }
    }
    foreach ($flavor in @('ucrt', 'um')) {
        $dir = Join-Path (Join-Path $sdk.LibDir $flavor) 'x64'
        if (Test-Path $dir) { $env:LIB = "$dir;$env:LIB" }
    }

    # --- 3. Rust / .NET -------------------------------------------------------
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    $dotnetDir = 'C:\Program Files\dotnet'
    foreach ($dir in @($cargoBin, $dotnetDir)) {
        if (-not (Test-Path $dir)) { throw "필수 도구 디렉터리가 없다: $dir" }
    }
    $env:PATH = "$cargoBin;$dotnetDir;$env:PATH"
    $env:DOTNET_CLI_TELEMETRY_OPTOUT = '1'
    $env:DOTNET_NOLOGO = '1'

    return [pscustomobject]@{
        RepoRoot   = $RepoRoot
        Msvc       = $msvc.FullName
        Linker     = (Get-Command link.exe).Source
        SdkSource  = $sdk.Source
        SdkVersion = $sdk.Version
        Cargo      = (Get-Command cargo.exe).Source
        Dotnet     = (Get-Command dotnet.exe).Source
    }
}

function Invoke-Checked {
    <#
      .SYNOPSIS
        명령을 실행하고 실패하면 즉시 예외를 던진다. 종료 코드만 판정한다.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [Parameter()][string[]]$ArgumentList = @(),
        [Parameter()][string]$What = ''
    )

    $label = if ($What) { $What } else { "$FilePath $($ArgumentList -join ' ')" }
    Write-Host "==> $label" -ForegroundColor Cyan

    & $FilePath @ArgumentList
    if ($LASTEXITCODE -ne 0) { throw "$label 실패 (exit $LASTEXITCODE)" }
}
