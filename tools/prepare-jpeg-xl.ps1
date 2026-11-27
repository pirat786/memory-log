param(
    [string]$Destination = "$PSScriptRoot/../target/release/codecs",
    [string]$ArchivePath = "$PSScriptRoot/../target/jpeg-xl-v0.13.0.zip"
)

$ErrorActionPreference = 'Stop'
$expectedHash = 'E394519C05EA9E5B83947F89B40AFBF1EA7C631A608406C5DE945A4C5BDCDCB7'
if (-not (Test-Path -LiteralPath $ArchivePath -PathType Leaf)) {
    New-Item -Path (Split-Path -Parent $ArchivePath) -ItemType Directory -Force | Out-Null
    $url = 'https://github.com/libjxl/libjxl/releases/download/v0.13.0/jxl-x64-windows-static.zip'
    Invoke-WebRequest -Uri $url -OutFile $ArchivePath
}
if ((Get-FileHash -LiteralPath $ArchivePath -Algorithm SHA256).Hash -ne $expectedHash) {
    throw 'JPEG XL archive checksum does not match the pinned libjxl release.'
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
$output = (New-Item -Path $Destination -ItemType Directory -Force).FullName
$licenses = (New-Item -Path (Join-Path $output 'licenses') -ItemType Directory -Force).FullName
$zip = [System.IO.Compression.ZipFile]::OpenRead((Resolve-Path -LiteralPath $ArchivePath).Path)
try {
    foreach ($name in 'cjxl.exe', 'djxl.exe') {
        $entry = $zip.GetEntry("x64-windows-static/bin/$name")
        if (-not $entry) { throw "Missing JPEG XL tool: $name" }
        [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $output $name), $true)
    }
    foreach ($entry in $zip.Entries) {
        if ($entry.FullName -match '^x64-windows-static/licenses/(LICENSE\.[A-Za-z0-9-]+)$') {
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $licenses $Matches[1]), $true)
        }
    }
} finally {
    $zip.Dispose()
}
