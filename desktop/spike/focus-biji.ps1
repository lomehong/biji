Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32 {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
}
"@
$p = Get-Process biji-gui
$h = $p.MainWindowHandle
[Win32]::ShowWindow($h, 9) | Out-Null   # SW_RESTORE
[Win32]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 400
$fg = [Win32]::GetForegroundWindow()
"foreground is biji: $($fg -eq $h) (fg=$fg, target=$h)"
