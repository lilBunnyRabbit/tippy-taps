param([Parameter(Mandatory)][string]$Base, [Parameter(Mandatory)][string]$Head, [Parameter(Mandatory)][string]$Out, [int]$Runs = 3, [int]$Seconds = 60)
$builds = [ordered]@{ base = $Base; head = $Head }
$scenarios = @(@{ w = 'hidden'; m = $false }, @{ w = 'hidden'; m = $true }, @{ w = 'open'; m = $false }, @{ w = 'open'; m = $true })
$data = Join-Path (Split-Path $Out) 'data'
for ($r = 1; $r -le $Runs; $r++) {
    foreach ($s in $scenarios) {
        foreach ($b in $builds.Keys) {
            $dir = Join-Path $data "$b-$($s.w)-$($s.m)-$r"
            Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
            "run $r $b $($s.w) move=$($s.m) $(Get-Date -Format HH:mm:ss)"
            & "$PSScriptRoot\sample.ps1" -Exe $builds[$b] -Data $dir -Out $Out -Build $b -Window $s.w -Move:$s.m -Seconds $Seconds -Run $r
        }
    }
}
