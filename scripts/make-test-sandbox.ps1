<#
.SYNOPSIS
  Build a disposable test sandbox for CleanUpStorages: two "drives" with duplicates,
  photos, a zip archive (with a duplicate inside), and a nested zip.

.DESCRIPTION
  Everything is created under a single throwaway root (default: Documents\cleanup-sandbox).
  Nothing here is your real data. Delete the whole folder when you're done.

  Layout created:
    <root>\
      catalog\                         <- isolated catalog dir (use as CLEANUPSTORAGES_DATA_DIR)
      DriveA\
        notes.txt                      unique
        report.txt                     "SHARED REPORT..."   (duplicate group, 4 copies total)
        copy\report.txt                "SHARED REPORT..."   exact loose duplicate
        photos\sunset.png              a real 64x64 image    (duplicate group, 2 copies)
        photos\sunset_copy.png         identical image
        bundle.zip                     contains keep.txt + report.txt(=SHARED REPORT)
        nested.zip                     contains inner.zip which contains deep.txt
      DriveB\
        report.txt                     "SHARED REPORT..."   cross-drive duplicate (a survivor)
        misc.txt                       unique

.PARAMETER Root
  Where to create the sandbox. Default: <Documents>\cleanup-sandbox
#>
param(
  [string]$Root = (Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'cleanup-sandbox')
)

$ErrorActionPreference = 'Stop'

# The exact bytes shared by every "report" copy (loose x3 across drives + inside the zip).
$Shared = "SHARED REPORT CONTENT v1 - this line makes several files identical`n"

Write-Host "Creating test sandbox at: $Root" -ForegroundColor Cyan
if (Test-Path $Root) { Remove-Item -Recurse -Force $Root }
foreach ($d in @('catalog','DriveA\copy','DriveA\photos','DriveB','_stage')) {
  New-Item -ItemType Directory -Force (Join-Path $Root $d) | Out-Null
}

function Put([string]$rel, [string]$text) {
  # Write bytes explicitly (no BOM) so identical text => identical bytes => identical hash.
  $path = Join-Path $Root $rel
  New-Item -ItemType Directory -Force (Split-Path $path) | Out-Null
  [System.IO.File]::WriteAllText($path, $text, (New-Object System.Text.UTF8Encoding($false)))
}

# --- loose files -------------------------------------------------------------
Put 'DriveA\notes.txt'       "personal notes - a unique file`n"
Put 'DriveA\report.txt'      $Shared
Put 'DriveA\copy\report.txt' $Shared      # exact duplicate of DriveA\report.txt
Put 'DriveB\report.txt'      $Shared      # cross-drive duplicate (survives on another drive)
Put 'DriveB\misc.txt'        "another unique file`n"

# --- two identical photos (a real 64x64 PNG so thumbnails show in the review GUI)
Add-Type -AssemblyName System.Drawing
$bmp = New-Object System.Drawing.Bitmap 64,64
$g   = [System.Drawing.Graphics]::FromImage($bmp)
$g.Clear([System.Drawing.Color]::CornflowerBlue)
$g.FillEllipse([System.Drawing.Brushes]::Gold, 12,12,40,40)
$bmp.Save((Join-Path $Root 'DriveA\photos\sunset.png'), [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Copy-Item (Join-Path $Root 'DriveA\photos\sunset.png') (Join-Path $Root 'DriveA\photos\sunset_copy.png')

# --- bundle.zip: keep.txt + a report.txt whose content == the loose report.txt
$stage = Join-Path $Root '_stage\bundle'
New-Item -ItemType Directory -Force $stage | Out-Null
[System.IO.File]::WriteAllText((Join-Path $stage 'keep.txt'), "keep me inside the archive`n", (New-Object System.Text.UTF8Encoding($false)))
[System.IO.File]::WriteAllText((Join-Path $stage 'report.txt'), $Shared, (New-Object System.Text.UTF8Encoding($false)))
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath (Join-Path $Root 'DriveA\bundle.zip') -Force

# --- nested.zip: inner.zip (which contains deep.txt)
$inner = Join-Path $Root '_stage\inner'
New-Item -ItemType Directory -Force $inner | Out-Null
[System.IO.File]::WriteAllText((Join-Path $inner 'deep.txt'), "a deeply nested file`n", (New-Object System.Text.UTF8Encoding($false)))
Compress-Archive -Path (Join-Path $inner 'deep.txt') -DestinationPath (Join-Path $Root '_stage\inner.zip') -Force
$outer = Join-Path $Root '_stage\outer'
New-Item -ItemType Directory -Force $outer | Out-Null
Move-Item (Join-Path $Root '_stage\inner.zip') (Join-Path $outer 'inner.zip')
Compress-Archive -Path (Join-Path $outer 'inner.zip') -DestinationPath (Join-Path $Root 'DriveA\nested.zip') -Force

Remove-Item -Recurse -Force (Join-Path $Root '_stage')

Write-Host "Done." -ForegroundColor Green
Write-Host ""
Write-Host "Two test drives:  $Root\DriveA   and   $Root\DriveB"
Write-Host "Isolated catalog: $Root\catalog   (set CLEANUPSTORAGES_DATA_DIR to this)"
Write-Host ""
Write-Host "Duplicate group (SHARED REPORT): DriveA\report.txt, DriveA\copy\report.txt,"
Write-Host "  DriveB\report.txt, and bundle.zip>report.txt  (4 copies)"
Write-Host "Photo duplicate: photos\sunset.png == photos\sunset_copy.png"
Write-Host "Nested archive:  nested.zip > inner.zip > deep.txt"
