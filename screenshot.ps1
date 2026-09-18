Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public class Win32 {
    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }
}
'@

$p = Get-Process -Name 'woolbrush-v3' -ErrorAction SilentlyContinue
if ($p -and $p.MainWindowHandle -ne 0) {
    $rect = New-Object Win32+RECT
    [void][Win32]::GetWindowRect($p.MainWindowHandle, [ref]$rect)
    $width = $rect.Right - $rect.Left
    $height = $rect.Bottom - $rect.Top
    Write-Output ("Window: ${width}x${height} at ($($rect.Left),$($rect.Top))")
    $bmp = New-Object System.Drawing.Bitmap($width, $height)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bmp.Size)
    $screenshotPath = 'C:\Users\liguo\woolbrush_screenshot.png'
    $bmp.Save($screenshotPath, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose()
    $bmp.Dispose()
    Write-Output ("Screenshot saved: $screenshotPath")
} else {
    Write-Output 'No window handle found'
}
