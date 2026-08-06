<#
.SYNOPSIS
  Build the fixed many-small-files corpus used to benchmark the scan's per-file SQLite write
  path (#26). Deterministic: same parameters => same file count, names, and byte content.

.DESCRIPTION
  #26 is about *per-file* write overhead (cached statements, synchronous=NORMAL, a byte-bounded
  commit trigger), so the corpus that matters is many small files, not a few large ones. This
  builds ~50,000 files of mixed small sizes (0 B .. ~256 KiB, weighted toward the small end so it
  resembles the "88.3% under 64 KB" shape documented in docs/benchmarking-scans.md) spread across
  a directory tree deep/wide enough that the walk phase isn't trivially one syscall.

  Everything is created under a single throwaway root, never the user's real data. Delete the
  whole folder when done. Re-running with the same -FileCount rebuilds byte-identical content
  (a seeded PRNG drives both the size and the content of every file), though mtimes will differ.

.PARAMETER Root
  Where to create the corpus. Default: <Documents>\cleanup-write-path-corpus

.PARAMETER FileCount
  Total number of files to generate. Default 50000 (within the 40,000-60,000 target range).

.PARAMETER Seed
  PRNG seed. Keep it fixed across rebuilds for a reproducible corpus.
#>
param(
  [string]$Root = (Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'cleanup-write-path-corpus'),
  [int]$FileCount = 50000,
  [int]$Seed = 26
)

$ErrorActionPreference = 'Stop'

Write-Host "Building write-path benchmark corpus at: $Root" -ForegroundColor Cyan
Write-Host "  FileCount = $FileCount, Seed = $Seed"

if (Test-Path $Root) {
  Write-Host "  Removing existing corpus..." -ForegroundColor Yellow
  Remove-Item -Recurse -Force $Root
}
New-Item -ItemType Directory -Force $Root | Out-Null

$rng = New-Object System.Random($Seed)

# 40 directories x 25 subdirectories = 1000 leaf dirs, ~50 files/leaf dir at FileCount=50000.
# Deep/wide enough that `walk` does real directory traversal, not one giant readdir.
$dirCount = 40
$subdirCount = 25
$leafDirs = New-Object 'System.Collections.Generic.List[string]'
for ($d = 0; $d -lt $dirCount; $d++) {
  for ($s = 0; $s -lt $subdirCount; $s++) {
    $rel = Join-Path ("dir{0:D3}" -f $d) ("sub{0:D3}" -f $s)
    $full = Join-Path $Root $rel
    New-Item -ItemType Directory -Force $full | Out-Null
    $leafDirs.Add($full) | Out-Null
  }
}

# Size distribution weighted toward small files, mirroring the real corpora this project scans:
#   60% 0-4 KiB, 25% 4-64 KiB, 12% 64 KiB-1 MiB, 3% 1-16 MiB.
function Get-RandomSize([System.Random]$r) {
  $roll = $r.NextDouble()
  if ($roll -lt 0.60) { return $r.Next(0, 4096) }
  elseif ($roll -lt 0.85) { return $r.Next(4096, 65536) }
  elseif ($roll -lt 0.97) { return $r.Next(65536, 1048576) }
  else { return $r.Next(1048576, 16777216) }
}

# Exact-size array per file, filled with NextBytes in one call (O(size), no PowerShell
# range-slice copy -- `$buffer[0..($size-1)]` builds a new array one element at a time via the
# pipeline, which is O(size) per element and made the first version of this script unusably slow
# for files past a few KB). A fresh small array per file is cheap; .NET's gen-0 GC eats this for
# breakfast, and it keeps every file's bytes genuinely distinct.
$written = 0L
$sw = [System.Diagnostics.Stopwatch]::StartNew()

for ($i = 0; $i -lt $FileCount; $i++) {
  $dir = $leafDirs[$i % $leafDirs.Count]
  $size = Get-RandomSize $rng
  $path = Join-Path $dir ("file{0:D6}.bin" -f $i)

  if ($size -gt 0) {
    $bytes = New-Object byte[] $size
    $rng.NextBytes($bytes)
    [System.IO.File]::WriteAllBytes($path, $bytes)
  } else {
    [System.IO.File]::WriteAllBytes($path, [byte[]]@())
  }
  $written += $size

  if (($i % 5000) -eq 0 -and $i -gt 0) {
    Write-Host ("  {0}/{1} files ({2:N1} MB so far, {3:N0} ms)" -f $i, $FileCount, ($written / 1MB), $sw.ElapsedMilliseconds)
  }
}

$sw.Stop()
Write-Host ""
Write-Host "Done: $FileCount files, $([Math]::Round($written / 1MB, 1)) MB, in $($sw.Elapsed)" -ForegroundColor Green
Write-Host "Corpus root: $Root"
Write-Host "Leaf directories: $($leafDirs.Count) ($dirCount x $subdirCount)"
