param([Parameter(Mandatory)][string]$Exe, [Parameter(Mandatory)][string]$Data, [int]$Runs = 5)
$env:APPDATA = $Data
foreach ($r in 1..$Runs) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process -FilePath $Exe -PassThru
    while ($p.MainWindowHandle -eq 0 -and $clock.ElapsedMilliseconds -lt 30000) {
        Start-Sleep -Milliseconds 1
        $p.Refresh()
    }
    $ms = $clock.ElapsedMilliseconds
    Start-Sleep -Seconds 3
    $p.Refresh()
    '{0},{1},{2:F1}' -f $r, $ms, ($p.WorkingSet64 / 1MB)
    Stop-Process -Id $p.Id -Force
    Start-Sleep -Seconds 1
}
