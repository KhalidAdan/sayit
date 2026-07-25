# sayit smoke test: synthesized push-to-talk against a running app.
#
# CONTAINMENT RULE: this script opens a scratch Notepad and focuses it
# BEFORE touching F9, because a working pipeline pastes into whatever has
# focus. Never simulate the hotkey without containment (docs/test-plan.md,
# regression ledger).
#
# Usage: powershell -File scripts\smoke-input.ps1 [-HoldMs 2500]
# Expects: sayit already running (npm run tauri dev) and warm.
# Pass: app log shows  sound: press -> captured Ns -> transcribed "...".

param([int]$HoldMs = 2500, [int]$Takes = 3)

# THREE takes by default, not one: the sleep-timer deadlock (regression
# ledger) only appeared on the SECOND take after a completed first — a
# single-take smoke passed for a day while the app was broken.
$scratch = Start-Process cmd -PassThru
Start-Sleep -Milliseconds 1500

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public class SmokeKbd {
    [DllImport("user32.dll")]
    public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
}
'@

$F9 = 0x78
for ($i = 1; $i -le $Takes; $i++) {
    [SmokeKbd]::keybd_event($F9, 0, 0, [UIntPtr]::Zero)   # key down
    Start-Sleep -Milliseconds $HoldMs
    [SmokeKbd]::keybd_event($F9, 0, 2, [UIntPtr]::Zero)   # key up
    Start-Sleep -Milliseconds 6000                         # let the pipeline finish
}

# Kill ONLY our scratch window (a name-based kill once murdered the vite
# shell tree and everything under it).
Stop-Process -Id $scratch.Id -Force -ErrorAction SilentlyContinue

Write-Host "Done. Check the sayit log for: sound: press -> captured -> transcribed."
Write-Host "Anything transcribed from room audio was pasted into the scratch Notepad."
Write-Host "Close it without saving. (Modern Notepad may respawn under a new PID.)"
