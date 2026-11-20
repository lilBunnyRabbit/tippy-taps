param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][string]$Data,
    [Parameter(Mandatory)][string]$Out,
    [string]$Build = 'build',
    [ValidateSet('hidden', 'open')][string]$Window = 'hidden',
    [switch]$Move,
    [int]$Seconds = 60,
    [int]$WarmUp = 8,
    [int]$Run = 1
)
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class Io {
    [StructLayout(LayoutKind.Sequential)] public struct IO_COUNTERS { public ulong ReadOps, WriteOps, OtherOps, ReadBytes, WriteBytes, OtherBytes; }
    [DllImport("kernel32.dll")] public static extern bool GetProcessIoCounters(IntPtr h, out IO_COUNTERS c);
}
'@
$scenario = "$Window-" + $(if ($Move) { 'moving' } else { 'idle' })
$env:APPDATA = $Data
New-Item -ItemType Directory -Force $Data | Out-Null
$launch = if ($Window -eq 'hidden') { @('--background') } else { @() }
$p = if ($launch.Count) { Start-Process -FilePath $Exe -ArgumentList $launch -PassThru } else { Start-Process -FilePath $Exe -PassThru }
Start-Sleep -Seconds $WarmUp
$mover = $null
if ($Move) {
    $mover = Start-Process powershell -ArgumentList '-NoProfile', '-File', "$PSScriptRoot\mover.ps1", '-Seconds', ($Seconds + 2) -PassThru -WindowStyle Hidden
    Start-Sleep -Milliseconds 1500
}
if (-not (Test-Path $Out)) { 'build,scenario,run,t,cpu_pct,ws_mb,private_mb,handles,threads,io_read_bytes,io_write_bytes' | Out-File -Encoding utf8 $Out }
$p.Refresh()
$io = New-Object Io+IO_COUNTERS
[void][Io]::GetProcessIoCounters($p.Handle, [ref]$io)
$lastCpu = $p.TotalProcessorTime.TotalMilliseconds; $lastR = $io.ReadBytes; $lastW = $io.WriteBytes
$clock = [Diagnostics.Stopwatch]::StartNew(); $lastT = 0
for ($t = 1; $t -le $Seconds; $t++) {
    while ($clock.ElapsedMilliseconds -lt $t * 1000) { Start-Sleep -Milliseconds 20 }
    $p.Refresh()
    [void][Io]::GetProcessIoCounters($p.Handle, [ref]$io)
    $now = $clock.ElapsedMilliseconds
    $cpu = $p.TotalProcessorTime.TotalMilliseconds
    $pct = ($cpu - $lastCpu) / ($now - $lastT) * 100
    '{0},{1},{2},{3},{4:F3},{5:F2},{6:F2},{7},{8},{9},{10}' -f $Build, $scenario, $Run, $t, $pct, ($p.WorkingSet64 / 1MB), ($p.PrivateMemorySize64 / 1MB), $p.HandleCount, $p.Threads.Count, ($io.ReadBytes - $lastR), ($io.WriteBytes - $lastW) | Out-File -Append -Encoding utf8 $Out
    $lastCpu = $cpu; $lastT = $now; $lastR = $io.ReadBytes; $lastW = $io.WriteBytes
}
if ($mover) { $mover.WaitForExit(10000) | Out-Null }
Stop-Process -Id $p.Id -Force
Start-Sleep -Seconds 2
