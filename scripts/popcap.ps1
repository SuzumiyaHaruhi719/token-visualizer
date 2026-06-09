# popcap.ps1 — find the claude-monitor "Today" popover, optionally move+resize it, capture it.
#   -W -H   : resize (device px). 0 = leave current size.
#   -X -Y   : move top-left (device px). Use -1 -1 to leave current position.
#   -Out    : output PNG path
#   -Settle : ms to wait after resize before capture (ResizeObserver + CSS transitions)
# Targets the window by OwnerPID(claude-monitor) + class "Tauri Window" + title "Today",
# so it can never grab an unrelated same-titled window.
param(
  [int]$W = 0,
  [int]$H = 0,
  [int]$X = -1,
  [int]$Y = -1,
  [string]$Out = "C:\Users\Thomas\Documents\Projects\claude-monitor\shots\pop.png",
  [int]$Settle = 700
)

Add-Type -AssemblyName System.Drawing

$sig = @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public class Win32Cap {
  [DllImport("user32.dll")]
  public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr l);
  public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr l);
  [DllImport("user32.dll")]
  public static extern bool IsWindowVisible(IntPtr hWnd);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)]
  public static extern int GetWindowText(IntPtr hWnd, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)]
  public static extern int GetClassName(IntPtr hWnd, StringBuilder s, int n);
  [DllImport("user32.dll")]
  public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
  [DllImport("user32.dll")]
  public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll")]
  public static extern bool SetWindowPos(IntPtr hWnd, IntPtr after, int X, int Y, int cx, int cy, uint flags);
  [DllImport("user32.dll")]
  public static extern IntPtr SetForegroundWindow(IntPtr hWnd);
  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int Left, Top, Right, Bottom; }
}
'@
if (-not ("Win32Cap" -as [type])) { Add-Type -TypeDefinition $sig }

$cmPid = (Get-Process claude-monitor -ErrorAction SilentlyContinue | Select-Object -First 1).Id
if (-not $cmPid) { Write-Error "claude-monitor not running"; exit 1 }

$script:found = [IntPtr]::Zero
$cb = [Win32Cap+EnumWindowsProc]{
  param($hw, $l)
  if (-not [Win32Cap]::IsWindowVisible($hw)) { return $true }
  $procId = 0
  [void][Win32Cap]::GetWindowThreadProcessId($hw, [ref]$procId)
  if ($procId -ne $cmPid) { return $true }
  $t = New-Object System.Text.StringBuilder 256
  [void][Win32Cap]::GetWindowText($hw, $t, 256)
  $c = New-Object System.Text.StringBuilder 256
  [void][Win32Cap]::GetClassName($hw, $c, 256)
  if ($t.ToString() -eq "Today" -and $c.ToString() -eq "Tauri Window") {
    $script:found = $hw; return $false
  }
  return $true
}
[void][Win32Cap]::EnumWindows($cb, [IntPtr]::Zero)
$h = $script:found
if ($h -eq [IntPtr]::Zero) { Write-Error "Today popover (PID $cmPid) not found"; exit 1 }

$SWP_NOMOVE=0x0002; $SWP_NOSIZE=0x0001; $SWP_NOZORDER=0x0004; $SWP_NOACTIVATE=0x0010
$HWND_TOP=[IntPtr]::Zero

# Determine target geometry. Read current rect first.
$r0 = New-Object Win32Cap+RECT
[void][Win32Cap]::GetWindowRect($h, [ref]$r0)
$tx = if ($X -ge 0) { $X } else { $r0.Left }
$ty = if ($Y -ge 0) { $Y } else { $r0.Top }
$tw = if ($W -gt 0) { $W } else { $r0.Right - $r0.Left }
$th = if ($H -gt 0) { $H } else { $r0.Bottom - $r0.Top }

# Apply geometry ONLY when a geom arg is given. We pass an explicit cx/cy every
# time (never SWP_NOSIZE): on this transparent undecorated Tauri/WebView2 window,
# a NOSIZE SetWindowPos makes the OS recompute the frame height to a bogus 65535.
# Passing the real size avoids that. With no geom args we do NOT touch the window
# at all (no raise) — same reason — and just read its current rect.
if ($W -gt 0 -or $H -gt 0 -or $X -ge 0 -or $Y -ge 0) {
  [void][Win32Cap]::SetWindowPos($h, $HWND_TOP, $tx, $ty, $tw, $th, $SWP_NOACTIVATE)
  Start-Sleep -Milliseconds $Settle
}

# Re-read the rect AFTER the resize (the window enforces its own min/max, so the
# actual size may differ from what we asked for).
$r = New-Object Win32Cap+RECT
[void][Win32Cap]::GetWindowRect($h, [ref]$r)
$rw = $r.Right - $r.Left
$rh = $r.Bottom - $r.Top
if ($rw -le 0 -or $rh -le 0 -or $rh -gt 4000) { Write-Error "bad rect ${rw}x${rh}"; exit 1 }

$bmp = New-Object System.Drawing.Bitmap $rw, $rh
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object System.Drawing.Size $rw, $rh))
$dir = Split-Path -Parent $Out
if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output ("OK actual={0}x{1} at {2},{3} -> {4}" -f $rw, $rh, $r.Left, $r.Top, $Out)
