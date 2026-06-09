# Capture an explicit screen region (device px), regardless of window size.
param(
  [int]$X = 400, [int]$Y = 200, [int]$W = 320, [int]$H = 460,
  [string]$Out = "C:\Users\Thomas\Documents\Projects\claude-monitor\shots\region.png"
)
Add-Type -AssemblyName System.Drawing
$bmp = New-Object System.Drawing.Bitmap $W, $H
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($X, $Y, 0, 0, (New-Object System.Drawing.Size $W, $H))
$dir = Split-Path -Parent $Out
if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output ("region {0}x{1} at {2},{3} -> {4}" -f $W, $H, $X, $Y, $Out)
