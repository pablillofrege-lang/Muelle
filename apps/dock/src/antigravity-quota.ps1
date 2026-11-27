$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$t3 = Join-Path ([Environment]::GetFolderPath('UserProfile')) '.t3'
$cliRoot = [regex]::Escape((Join-Path $t3 'tools\antigravity-acp\win32-x64\versions'))
$profileRoot = [regex]::Escape((Join-Path $t3 'userdata\providers\antigravity'))
$clients = @(Get-CimInstance Win32_Process -Filter "Name='agy.exe'" -OperationTimeoutSec 5 | ForEach-Object {
    if ($_.ExecutablePath -notmatch "^$cliRoot\\[0-9a-f]{64}\\agy\.exe$") { return }
    if ($_.CommandLine -notmatch '(?:^|\s)acp(?:\s|$)') { return }
    if ($_.CommandLine -notmatch '(?:^|\s)--app-data-dir(?:=|\s+)("?antigravity-acp"?)(?:\s|$)') { return }
    if ($_.CommandLine -notmatch '(?:^|\s)--gemini-dir(?:=|\s+)(?:"([^"]+)"|(\S+))') { return }
    $profilePath = $Matches[1] + $Matches[2]
    if ($profilePath -match "^$profileRoot\\[0-9a-f]{64}$") {
        [pscustomobject]@{ ProfilePath = $profilePath; Executable = $_.ExecutablePath }
    }
})
if ($clients.Count -eq 0 -or @($clients.ProfilePath | Sort-Object -Unique).Count -ne 1) { exit 1 }
$client = $clients[0]
$credential = Join-Path $client.ProfilePath 'antigravity-acp\antigravity-oauth-token'
$lock = [IO.File]::Open($credential, 'Open', 'Read', 'Read')
try {
    $env:ANTIGRAVITY_GEMINI_DIR = $client.ProfilePath
    $env:ANTIGRAVITY_APP_DATA_DIR = 'antigravity-acp'
    $output = & $client.Executable --print /usage --output-format json --print-timeout 8s 2>$null
    if ($LASTEXITCODE -ne 0) { exit 1 }
    $result = ($output -join "`n") | ConvertFrom-Json
    if ($result.command.name -ne 'usage' -or $result.num_turns -ne 0) { exit 1 }
    @{command=@{name='usage'; data=@{groups=$result.command.data.groups}}} | ConvertTo-Json -Depth 10 -Compress
} finally {
    $lock.Dispose()
}
