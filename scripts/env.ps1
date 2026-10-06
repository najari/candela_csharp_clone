# ---------------------------------------------------------------------------
# DiagAuthor 개발 환경 진입 스크립트.
#
#   pwsh -NoProfile -Command ". .\scripts\env.ps1"
#
# 이 머신의 제약( scripts\common.ps1 머리말 참고):
#   .NET SDK 와 Rust 도구체인이 PATH 에 없고, Windows SDK Include/Lib 도
#   시스템에 없다. 이 스크립트는 현재 PowerShell 세션에 그 경로들을 넣고
#   끝난다. 빌드·테스트는 build.ps1 / test.ps1 이 스스로 환경을 만들기 때문에
#   이 스크립트는 "cargo test 를 손으로 돌리겠다" 할 때만 필요하다.
#
# 다른 세션(새 터미널, VS Code, 탐색기)에서 쓰려면 사용자 단위 PATH 에
# 영구 등록한다. 저장소를 옮기거나 되돌릴 때 지우려면 같은 스크립트를
# -Unregister 로 다시 실행한다.
#
#   pwsh -NoProfile -File .\scripts\env.ps1 -Register
#   pwsh -NoProfile -File .\scripts\env.ps1 -Unregister
# ---------------------------------------------------------------------------

[CmdletBinding()]
param(
    # 사용자 단위 PATH 에 영구 등록한다(새 세션부터 유효).
    [switch]$Register,
    # 등록된 항목을 제거한다.
    [switch]$Unregister
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'common.ps1')

$repoRoot = Split-Path -Parent $PSScriptRoot
# Cargo bin 은 사용자 홈 아래라 사용자별 경로가 맞고, .NET 은 Program Files 로
# 고정이라 머신별 경로가 맞다. 둘 다 사용자 PATH 에만 쓴다.
$marker = 'C:\Users\admin\.cargo\bin'
$dotnetDir = 'C:\Program Files\dotnet'
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')

# PATH 는 ';' 로 구분한다. Windows 는 빈 항목을 "현재 디렉터리"로 해석하므로
# 쓰기 전에 빈 항목을 모두 걷어내고, 결과가 빈 문자열이면 제거 대신 빈 문자열을
# 그대로 쓴다(그래야 환경 변수 자체가 사라진다).
function Get-PathEntries {
    param([string]$Value)
    if ([string]::IsNullOrEmpty($Value)) { return @() }
    return @($Value.Split(';') | Where-Object { $_ -ne '' })
}

function Remove-Marker {
    param([string]$Value)
    $kept = @(Get-PathEntries $Value | Where-Object { $_.Trim() -ne $marker })
    if ($kept.Count -eq 0) { return '' }
    return ($kept -join ';')
}

if ($Unregister) {
    $entries = @(Get-PathEntries $userPath)
    if ($entries.Count -eq 0 -or -not ($entries | Where-Object { $_.Trim() -eq $marker })) {
        Write-Host '[env] 사용자 PATH 에 등록된 항목이 없다. 아무것도 하지 않는다.' -ForegroundColor Yellow
        exit 0
    }
    [Environment]::SetEnvironmentVariable('Path', (Remove-Marker $userPath), 'User')
    Write-Host "[env] 사용자 PATH 에서 제거했다: $marker" -ForegroundColor Green
    Write-Host '[env] 새 세션부터 반영된다.' -ForegroundColor DarkGray
    exit 0
}

try {
    $envInfo = Initialize-DiagAuthorEnvironment -RepoRoot $repoRoot
}
catch {
    Write-Host "[env] 준비 실패: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}

# 현재 세션 반영
$env:PATH = "$($envInfo.Msvc)\bin\Hostx64\x64;C:\Users\admin\.cargo\bin;$dotnetDir;$env:PATH"
$env:DOTNET_CLI_TELEMETRY_OPTOUT = '1'
$env:DOTNET_NOLOGO = '1'

Write-Host '[env] 현재 세션에 다음을 반영했다.' -ForegroundColor Green
Write-Host "  msvc   : $($envInfo.Msvc)" -ForegroundColor DarkGray
Write-Host "  sdk    : $($envInfo.SdkSource) $($envInfo.SdkVersion)" -ForegroundColor DarkGray
Write-Host "  cargo  : $($envInfo.Cargo)" -ForegroundColor DarkGray
Write-Host "  dotnet : $($envInfo.Dotnet)" -ForegroundColor DarkGray

if ($Register) {
    $entries = @(Get-PathEntries $userPath)
    if ($entries.Count -eq 0) {
        [Environment]::SetEnvironmentVariable('Path', $marker, 'User')
        Write-Host "[env] 사용자 PATH 에 등록했다: $marker" -ForegroundColor Green
    }
    elseif ($entries | Where-Object { $_.Trim() -eq $marker }) {
        Write-Host "[env] 이미 사용자 PATH 에 있다: $marker" -ForegroundColor DarkGray
    }
    else {
        [Environment]::SetEnvironmentVariable('Path', (@($marker) + $entries -join ';'), 'User')
        Write-Host "[env] 사용자 PATH 에 등록했다: $marker" -ForegroundColor Green
    }
    if (-not (Test-Path (Join-Path $dotnetDir 'dotnet.exe'))) {
        Write-Host "[env] 경고: $dotnetDir 에 dotnet.exe 가 없다. .NET SDK 설치가 필요하다." -ForegroundColor Yellow
    }
    Write-Host '[env] 새 세션부터 cargo·dotnet 이 PATH 에 보인다.' -ForegroundColor DarkGray
}
else {
    Write-Host '[env] 영구 등록은 하지 않았다. -Register 를 붙이면 사용자 PATH 에 추가한다.' -ForegroundColor Yellow
}
