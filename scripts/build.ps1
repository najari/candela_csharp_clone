# ---------------------------------------------------------------------------
# DiagAuthor 전체 빌드.
#   cargo build --release   -> target\release\diagauthor-engine.exe
#   dotnet build -c Release -> gui\DiagAuthor.Desktop\bin\Release\...
#
# 어느 단계든 실패하면 예외를 던져 종료 코드가 0 이 아니게 만든다.
# ---------------------------------------------------------------------------

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'common.ps1')

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

try {
    $envInfo = Initialize-DiagAuthorEnvironment -RepoRoot $repoRoot
    Write-Host "[build] msvc   : $($envInfo.Msvc)" -ForegroundColor DarkGray
    Write-Host "[build] linker : $($envInfo.Linker)" -ForegroundColor DarkGray
    Write-Host "[build] sdk    : $($envInfo.SdkSource) $($envInfo.SdkVersion)" -ForegroundColor DarkGray
    Write-Host "[build] cargo  : $($envInfo.Cargo)" -ForegroundColor DarkGray
    Write-Host "[build] dotnet : $($envInfo.Dotnet)" -ForegroundColor DarkGray

    Invoke-Checked -FilePath $envInfo.Cargo `
        -ArgumentList @('build', '--release') `
        -What 'cargo build --release'

    Invoke-Checked -FilePath $envInfo.Dotnet `
        -ArgumentList @('build', 'DiagAuthor.slnx', '-c', 'Release') `
        -What 'dotnet build -c Release'

    Write-Host '[build] 완료' -ForegroundColor Green
    exit 0
}
catch {
    Write-Host "[build] 실패: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
