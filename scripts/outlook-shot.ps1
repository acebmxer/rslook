# Screenshots the Outlook classic main window ONLY (by window handle, never the desktop), for
# black-box UI comparison. Output stays in plan/outlook/screenshots/ (gitignored; never commit it).
#
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts\outlook-shot.ps1 -Name home-tab
#
# Uses PrintWindow, so the window may be covered by other windows, but it must not be minimised.
param(
    [Parameter(Mandatory = $true)][string]$Name,
    [string]$OutDir = ''
)
$ErrorActionPreference = 'Stop'
if (-not $OutDir) {
    $root = Split-Path -Parent $MyInvocation.MyCommand.Path
    $OutDir = Join-Path $root '..\plan\outlook\screenshots'
}
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
"@
[void][Win]::SetProcessDPIAware()
$p = Get-Process OUTLOOK -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { throw 'Outlook is not running (or has no main window yet).' }
$h = $p.MainWindowHandle
if ([Win]::IsIconic($h)) { throw 'Outlook is minimised; restore it first.' }
$r = New-Object Win+RECT
[void][Win]::GetWindowRect($h, [ref]$r)
$w = $r.Right - $r.Left; $hgt = $r.Bottom - $r.Top
$bmp = New-Object System.Drawing.Bitmap $w, $hgt
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
# 2 = PW_RENDERFULLCONTENT (captures GPU-composited content).
[void][Win]::PrintWindow($h, $hdc, 2)
$g.ReleaseHdc($hdc); $g.Dispose()
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$safe = ($Name -replace '[^\w\-]', '_')
$out = Join-Path (Resolve-Path $OutDir) ("{0}-{1}.png" -f (Get-Date -Format 'yyyyMMdd-HHmmss'), $safe)
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
Write-Output "$out  ($w x $hgt, title: $($p.MainWindowTitle))"
