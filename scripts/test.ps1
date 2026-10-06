# ---------------------------------------------------------------------------
# DiagAuthor 전체 테스트.
#   cargo test         -> 엔진 단위 + 통합 테스트
#   dotnet build       -> 데스크톱 프로젝트는 테스트 프로젝트가 없으므로
#                         빌드만으로 컴파일 검증을 대신한다(Plan M3 종료 조건).
#
# 어느 단계든 실패하면 예외를 던져 종료 코드가 0 이 아니게 만든다.
# 이 스크립트에서 EXE 를 만들지 않는다. 없는 산출물을 성공으로 보지 않는다.
# ---------------------------------------------------------------------------

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'common.ps1')

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

try {
    $envInfo = Initialize-DiagAuthorEnvironment -RepoRoot $repoRoot
    Write-Host "[test] sdk    : $($envInfo.SdkSource) $($envInfo.SdkVersion)" -ForegroundColor DarkGray
    Write-Host "[test] cargo  : $($envInfo.Cargo)" -ForegroundColor DarkGray
    Write-Host "[test] dotnet : $($envInfo.Dotnet)" -ForegroundColor DarkGray

    Invoke-Checked -FilePath $envInfo.Cargo `
        -ArgumentList @('test') `
        -What 'cargo test'

    # M0 종료 조건은 "빌드·테스트 스크립트가 0 으로 종료"이고, M3 종료 조건은
    # "dotnet build DiagAuthor.slnx"다. 테스트 프로젝트가 없으므로 --no-restore
    # 없이 복사까지 수행해 빌드 자체가 깨지는 경우를 잡는다.
    Invoke-Checked -FilePath $envInfo.Dotnet `
        -ArgumentList @('build', 'DiagAuthor.slnx', '-c', 'Release') `
        -What 'dotnet build -c Release (컴파일 검증)'

    Write-Host '[test] 완료' -ForegroundColor Green
    exit 0
}
catch {
    Write-Host "[test] 실패: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
