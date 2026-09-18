$ErrorActionPreference = 'Stop'
$root = Join-Path $env:RUNNER_TEMP ('AVT update 测试 ' + [guid]::NewGuid())
$stage = Join-Path $root '.avt-update-test'
New-Item -ItemType Directory -Path $stage -Force | Out-Null
$app = Join-Path $root 'AVT-Replenishment.exe'
Copy-Item target/release/AVT-Replenishment.exe $app
# A valid PE overlay gives the old file a distinct hash.
$stream = [IO.File]::Open($app, 'Append'); $stream.WriteByte(42); $stream.Dispose()
Copy-Item $app (Join-Path $stage 'old.exe')
Copy-Item target/release/AVT-Replenishment.exe (Join-Path $stage 'helper.exe')
Copy-Item target/release/AVT-Replenishment.exe (Join-Path $stage 'new.exe')
$expected = (Get-FileHash (Join-Path $stage 'new.exe')).Hash.ToLowerInvariant()
$old = Start-Process -FilePath $app -PassThru
$helper = $null
try {
  Start-Sleep -Seconds 2
  if ($old.HasExited) { throw 'Original program did not remain running' }
  $helper = Start-Process -FilePath (Join-Path $stage 'helper.exe') -ArgumentList @('--avt-install-update', ('"' + $app + '"'), $old.Id, $expected) -PassThru
  Start-Sleep -Seconds 1
  Stop-Process -Id $old.Id -Force
  if (-not $helper.WaitForExit(30000)) { throw 'Update helper did not finish' }
  $deadline = (Get-Date).AddSeconds(20)
  while ((Test-Path $stage) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 250 }
  if (Test-Path $stage) { throw 'Restarted program did not clean staging directory' }
  if ((Get-FileHash $app).Hash.ToLowerInvariant() -ne $expected) { throw 'Program was not replaced' }
  $running = @(Get-Process | Where-Object { $_.Path -eq $app })
  if ($running.Count -ne 1) { throw 'Updated program did not restart exactly once' }
  Write-Host 'Automatic update passed: waited for exit, replaced program, restarted, cleaned staging; Unicode/spaced paths.'
} finally {
  Get-Process | Where-Object { $_.Path -and $_.Path.StartsWith($root) } | Stop-Process -Force -ErrorAction SilentlyContinue
  Remove-Item $root -Recurse -Force -ErrorAction SilentlyContinue
}
