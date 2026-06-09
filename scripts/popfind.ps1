# popfind.ps1 — enumerate all visible top-level windows, show handle/title/class/rect/pid.
# Used to identify the real Tauri popover among any same-titled windows.
$sig = @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public class Win32Find {
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
  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int Left, Top, Right, Bottom; }
}
'@
if (-not ("Win32Find" -as [type])) { Add-Type -TypeDefinition $sig }

$rows = New-Object System.Collections.ArrayList
$cb = [Win32Find+EnumWindowsProc]{
  param($hw, $l)
  if (-not [Win32Find]::IsWindowVisible($hw)) { return $true }
  $t = New-Object System.Text.StringBuilder 256
  [void][Win32Find]::GetWindowText($hw, $t, 256)
  $title = $t.ToString()
  if ([string]::IsNullOrWhiteSpace($title)) { return $true }
  $c = New-Object System.Text.StringBuilder 256
  [void][Win32Find]::GetClassName($hw, $c, 256)
  $r = New-Object Win32Find+RECT
  [void][Win32Find]::GetWindowRect($hw, [ref]$r)
  $procId = 0
  [void][Win32Find]::GetWindowThreadProcessId($hw, [ref]$procId)
  [void]$rows.Add([pscustomobject]@{
    HWND = [int64]$hw
    Title = $title
    Class = $c.ToString()
    W = $r.Right - $r.Left
    H = $r.Bottom - $r.Top
    X = $r.Left; Y = $r.Top
    OwnerPID = $procId
  })
  return $true
}
[void][Win32Find]::EnumWindows($cb, [IntPtr]::Zero)
$rows | Sort-Object Title | Format-Table -AutoSize | Out-String -Width 200
