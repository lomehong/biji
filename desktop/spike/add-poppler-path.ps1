$dir = Join-Path $env:LOCALAPPDATA 'Microsoft\WinGet\Packages\oschwartz10612.Poppler_Microsoft.Winget.Source_8wekyb3d8bbwe\poppler-25.07.0\Library\bin'
$p = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($p -split ';' -notcontains $dir) {
    [Environment]::SetEnvironmentVariable('Path', ($p.TrimEnd(';') + ';' + $dir), 'User')
    Write-Output "PATH updated: added $dir"
} else {
    Write-Output 'already present'
}
