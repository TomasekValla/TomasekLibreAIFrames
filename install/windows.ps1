# Installs TomasekLibreAIFrames on Windows for the current user, no administrator rights needed:
#   irm https://raw.githubusercontent.com/TomasekValla/TomasekLibreAIFrames/main/install/windows.ps1 | iex
# Set $env:TLAF_PRERELEASE = "1" first to take the newest pre-release instead of the newest stable release.
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$repo = "TomasekValla/TomasekLibreAIFrames"
$app = "TomasekLibreAIFrames"
$name = "$app-windows-x64.zip"

if ($env:PROCESSOR_ARCHITECTURE -ne "AMD64") { throw "Only 64-bit x86 Windows is supported for now." }

$releases = Invoke-RestMethod "https://api.github.com/repos/$repo/releases?per_page=20" -Headers @{ "User-Agent" = "$app-installer" }
$releases = @($releases | Where-Object { -not $_.draft })
$stable = @($releases | Where-Object { -not $_.prerelease })
$release = if ($env:TLAF_PRERELEASE -eq "1" -or $stable.Count -eq 0) { $releases[0] } else { $stable[0] }
if (-not $release) { throw "No release found." }
$asset = $release.assets | Where-Object name -eq $name
$sum = $release.assets | Where-Object name -eq "$name.sha256"
if (-not $asset -or -not $sum) { throw "No $name in $($release.tag_name)." }

$tmp = Join-Path ([IO.Path]::GetTempPath()) "$app-install-$PID"
$dest = Join-Path $env:LOCALAPPDATA "Programs\$app"
New-Item -ItemType Directory -Force $tmp | Out-Null
try {
    Write-Host "Downloading $app $($release.tag_name)..."
    $zip = Join-Path $tmp $name
    Invoke-WebRequest $asset.browser_download_url -OutFile $zip -UseBasicParsing
    Invoke-WebRequest $sum.browser_download_url -OutFile "$zip.sha256" -UseBasicParsing
    $want = ((Get-Content "$zip.sha256" -Raw).Trim() -split '\s+')[0].ToLower()
    $got = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    if ($want -ne $got) { throw "Checksum mismatch, nothing was installed." }
    Write-Host "Checksum verified."

    Get-Process $app -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$dest\*" } | Stop-Process -Force
    Expand-Archive $zip -DestinationPath $tmp -Force
    $src = Join-Path $tmp "$app-windows-x64"
    New-Item -ItemType Directory -Force $dest | Out-Null
    Copy-Item "$src\*" $dest -Recurse -Force

    $exe = Join-Path $dest "$app.exe"
    $lnk = Join-Path ([Environment]::GetFolderPath("Programs")) "$app.lnk"
    $sh = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk)
    $sh.TargetPath = $exe
    $sh.WorkingDirectory = $dest
    $sh.IconLocation = "$exe,0"
    $sh.Save()

    Write-Host "$app $($release.tag_name) is installed in $dest. Find it in the Start menu."
    Start-Process $exe
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
