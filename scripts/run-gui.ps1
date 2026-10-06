# ---------------------------------------------------------------------------
# DiagAuthor WPF 셸 실행.
#
#   scripts\run-gui.cmd                 일반 실행
#   scripts\run-gui.cmd --smoke-test    앱 내장 스모크 하네스 후 종료
#
# 스모크는 M3 종료 조건의 검증 명령이다:
#   "dotnet build DiagAuthor.slnx" · "scripts\run-gui.cmd --smoke-test"
# 앱을 실제로 띄워 셸 렌더·프로젝트 생성·목록 가상화·저장 독을 확인하고
# 창 스크린샷과 JSON 리포트를 artifacts\ 에 남긴다. 브라우저나 콘솔 출력에
# 의존하지 않는다(계획서 8절).
# ---------------------------------------------------------------------------

[CmdletBinding()]
param(
    [switch]$SmokeTest,
    # 스모크 산출물을 쓸 디렉터리. 기본은 저장소 artifacts\.
    [string]$OutputDir
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'common.ps1')

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

if ([string]::IsNullOrWhiteSpace($OutputDir)) {
    # The app writes smoke-report.json directly into the directory it is given,
    # so the default is the run directory, not the parent artifacts\ folder.
    $OutputDir = Join-Path $repoRoot 'artifacts\smoke'
}
# Report freshness is guaranteed by deleting the previous report before launch.

$configuration = if ($SmokeTest) { 'Release' } else { 'Debug' }
$exe = Join-Path $repoRoot "gui\DiagAuthor.Desktop\bin\$configuration\net10.0-windows\win-x64\DiagAuthor.exe"

try {
    $envInfo = Initialize-DiagAuthorEnvironment -RepoRoot $repoRoot

    # 소스가 실행 파일보다 최신이면 그대로 실행하면 낡은 바이너리를 돌게 된다.
    # 이 실행에서 산출물이 없으면 성공으로 보지 않는다는 계획서 15절 원칙을
    # 여기에도 적용한다.
    $project = Join-Path $repoRoot 'gui\DiagAuthor.Desktop\DiagAuthor.Desktop.csproj'
    $stale = $false
    if (Test-Path $exe) {
        $exeTime = (Get-Item -LiteralPath $exe).LastWriteTimeUtc
        $sourceRoot = Join-Path $repoRoot 'gui\DiagAuthor.Desktop'
        $newer = Get-ChildItem -Path $sourceRoot -File -Recurse -Include '*.cs', '*.xaml', '*.csproj' |
            Where-Object { $_.FullName -notmatch '\\obj\\|\\bin\\' -and $_.LastWriteTimeUtc -gt $exeTime }
        $stale = $null -ne $newer
    }
    if (-not (Test-Path $exe) -or $stale) {
        $reason = if (Test-Path $exe) { '소스가 더 최신이다' } else { '실행 파일이 없다' }
        Write-Host "[run-gui] $reason. 먼저 빌드한다." -ForegroundColor Yellow
        Invoke-Checked -FilePath $envInfo.Dotnet `
            -ArgumentList @('build', $project, '-c', $configuration) `
            -What 'dotnet build'
    }
    if (-not (Test-Path $exe)) { throw "빌드 후에도 실행 파일이 없다: $exe" }

    # 엔진은 GUI 옆에 두지 않고 cargo 산출물에서 찾는다. 있으면 그 경로를,
    # 없으면 목업 모드로 넘어간다(계획서 3절 모드 결정).
    $engine = Join-Path $repoRoot 'target\release\diagauthor-engine.exe'
    if (-not (Test-Path $engine)) {
        $engine = Join-Path $repoRoot 'target\debug\diagauthor-engine.exe'
    }
    if (-not (Test-Path $engine)) {
        if ($SmokeTest) {
            # 스모크는 목업 모드 통과를 성공으로 세지 않는다. 계획서 8절은
            # "브라우저·콘솔 출력에 의존하지 않는다"고 하고, M3 종료 조건은
            # 엔진 hello 왕복을 요구한다.
            throw "스모크에 필요한 엔진 바이너리가 없다: target\release\diagauthor-engine.exe (cargo build 필요)"
        }
        Write-Host '[run-gui] 엔진 바이너리가 없다. 목업 모드로 실행한다.' -ForegroundColor Yellow
    }
    else {
        Write-Host "[run-gui] engine  : $engine" -ForegroundColor DarkGray
    }

    New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
    if ($SmokeTest) {
        # Clear the previous report so a crashed run cannot leave a passing one
        # behind for the freshness check to accept.
        Remove-Item -LiteralPath (Join-Path $OutputDir 'smoke-report.json') -ErrorAction SilentlyContinue
    }
    $arguments = @()
    if ($SmokeTest) {
        # The app's own default is artifacts\smoke. Pass the directory explicitly
        # so a caller-supplied -OutputDir is honoured rather than ignored.
        $arguments += '--smoke-test'
        $arguments += $OutputDir
    }

    Write-Host "[run-gui] exe     : $exe" -ForegroundColor DarkGray
    if ($SmokeTest) { Write-Host "[run-gui] output  : $OutputDir" -ForegroundColor DarkGray }

    # DiagAuthor.exe is OutputType=WinExe (GUI subsystem), so `& $exe` returns
    # immediately without waiting: PowerShell does not block on a non-console
    # process started this way. That would let the smoke report be read before
    # it is written, and a previous run's report would be mistaken for this
    # run's. Start-Process -Wait is the correct way to run it synchronously.
    #
    # -NoNewWindow is deliberately not used: it would make the GUI process (and
    # the engine child it spawns) inherit this shell's console handles, so a
    # caller that redirects output waits on those handles after the app exits.
    $process = Start-Process -FilePath $exe -ArgumentList $arguments -Wait -PassThru
    $code = $process.ExitCode

    if ($code -ne 0) { throw "DiagAuthor 실행 실패 (exit $code)" }

    if ($SmokeTest) {
        $report = Join-Path $OutputDir 'smoke-report.json'
        if (-not (Test-Path $report)) {
            throw "스모크 리포트가 생성되지 않았다: $report"
        }
        $parsed = Get-Content -LiteralPath $report -Raw | ConvertFrom-Json
        if (-not $parsed.passed) {
            $failed = @($parsed.steps | Where-Object { -not $_.passed } | ForEach-Object { $_.name })
            $detail = '실패 단계가 기록되지 않았다'
            if ($failed) { $detail = "실패 단계: " + ($failed -join ', ') }
            elseif ($parsed.error) { $detail = "원인: " + $parsed.error }
            throw "스모크 실패. $detail"
        }
        $stepCount = @($parsed.steps).Count
        # $parsed.delivery is empty in mock mode, where no engine is involved.
        $delivery = ''
        if ($parsed.delivery) { $delivery = ' 전달본=' + $parsed.delivery }
        Write-Host "[run-gui] 스모크 통과. 모드=$($parsed.mode) 단계=$stepCount 경과=$($parsed.elapsed_ms)ms$delivery" -ForegroundColor Green
    }
    else {
        Write-Host '[run-gui] 정상 종료' -ForegroundColor Green
    }
    exit 0
}
catch {
    Write-Host "[run-gui] 실패: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
