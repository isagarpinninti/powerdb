# Starts PowerDB at logon as a scheduled task (no admin rights needed).
# PowerDB is a plain console program, not a Windows Service; use NSSM or WinSW if you need a true service.
param([string]$Exe = "$env:LOCALAPPDATA\PowerDB\powerdb.exe", [string]$Data = "$env:LOCALAPPDATA\PowerDB\data")
$action  = New-ScheduledTaskAction -Execute $Exe -Argument "serve --data `"$Data`""
$trigger = New-ScheduledTaskTrigger -AtLogOn
Register-ScheduledTask -TaskName "PowerDB" -Action $action -Trigger $trigger -Force | Out-Null
Start-ScheduledTask -TaskName "PowerDB"
Write-Host "PowerDB scheduled to run at logon; listening on http://127.0.0.1:8090"
