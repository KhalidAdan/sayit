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

param([int]$HoldMs = 2500)

$notepad = Start-Process notepad -PassThru
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
[SmokeKbd]::keybd_event($F9, 0, 0, [UIntPtr]::Zero)   # key down
Start-Sleep -Milliseconds $HoldMs
[SmokeKbd]::keybd_event($F9, 0, 2, [UIntPtr]::Zero)   # key up
Start-Sleep -Milliseconds 3000                         # let the pipeline finish

Write-Host "Done. Check the sayit log for: sound: press -> captured -> transcribed."
Write-Host "Anything transcribed from room audio was pasted into the scratch Notepad."
Write-Host "Close it without saving. (Modern Notepad may respawn under a new PID.)"
