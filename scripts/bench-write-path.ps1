<#
.SYNOPSIS
  Repeatable A/B benchmark for the scan's per-file SQLite write path (#26).

.DESCRIPTION
  Measures BOTH code paths against the fixed corpus built by make-write-path-corpus.ps1:
    - `scan --force` over the corpus on a FRESH catalog: the hashing path (Phase::Hash dominates,
      every file is a new insert).
    - a second, plain `scan` (no --force) immediately after: the skip_check path (Phase::SkipCheck
      dominates, every file is already catalogued and unchanged).
  These are different code paths (see docs/benchmarking-scans.md, trap #3) and this script never
  averages or otherwise conflates them — both are reported, separately, every run.

  Each invocation of this script:
    - builds a FRESH temp CLEANUPSTORAGES_DATA_DIR (never the user's real one, never reused
      between runs, so no run inherits another run's catalog state or snapshot backlog);
    - never writes into the real %APPDATA%\justPrototype\CleanUpStorages\ directory;
    - records whether this is a cold or warm run of the OS file cache against the corpus, so
      figures cannot be silently mixed (trap #2).
    - notes whether the corpus folder appears to be Defender-excluded is NOT auto-detected --
      the caller must state it via -DefenderExcluded, and the script prints it prominently, so a
      figure can never be quoted without knowing which condition it was taken under (trap #1).

.PARAMETER Label
  Short label recorded in the output, identifying what this run is testing (e.g. "baseline-1",
  "cached-statements").

.PARAMETER CorpusRoot
  Path to the corpus built by make-write-path-corpus.ps1. Default matches that script's default.

.PARAMETER BinaryPath
  Path to the cleanupstorages binary to benchmark. Default: target\release\cleanupstorages.exe
  relative to the repo root (two levels up from this script). Always use a --release build --
  a debug build's timings are meaningless for this purpose.

.PARAMETER CacheCondition
  'cold' or 'warm'. This script cannot force a cold OS cache (that needs a reboot or a volume far
  larger than RAM) -- it only records what the caller asserts, so state honestly. Default 'warm'.

.PARAMETER DefenderExcluded
  Whether the corpus folder is currently excluded from Windows Defender. This script does not
  change Defender settings; it only records the condition (see docs/benchmarking-scans.md trap #1).

.EXAMPLE
  .\scripts\bench-write-path.ps1 -Label "baseline-1" -DefenderExcluded:$false
#>
param(
  [Parameter(Mandatory = $true)]
  [string]$Label,

  [string]$CorpusRoot = (Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'cleanup-write-path-corpus'),

  [string]$BinaryPath = (Join-Path $PSScriptRoot '..\target\release\cleanupstorages.exe'),

  [ValidateSet('cold', 'warm')]
  [string]$CacheCondition = 'warm',

  [Parameter(Mandatory = $true)]
  [bool]$DefenderExcluded
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path $CorpusRoot)) {
  throw "Corpus not found at $CorpusRoot. Run scripts\make-write-path-corpus.ps1 first."
}
$BinaryPath = (Resolve-Path $BinaryPath).Path
if (-not (Test-Path $BinaryPath)) {
  throw "Binary not found at $BinaryPath. Run 'cargo build --release' first."
}

# A fresh throwaway data dir per run -- never the user's real %APPDATA% catalog (#44). Created
# under the OS temp dir, well away from anything the app would resolve to by default.
$dataDir = Join-Path ([System.IO.Path]::GetTempPath()) ("cus-bench-{0}-{1}" -f $Label, [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force $dataDir | Out-Null
$env:CLEANUPSTORAGES_DATA_DIR = $dataDir

Write-Host "==================================================================" -ForegroundColor Cyan
Write-Host "Write-path benchmark: $Label" -ForegroundColor Cyan
Write-Host "==================================================================" -ForegroundColor Cyan
Write-Host "Corpus:              $CorpusRoot"
Write-Host "Binary:              $BinaryPath"
Write-Host "Data dir (fresh):    $dataDir"
Write-Host "Cache condition:     $CacheCondition  (asserted by caller, not auto-detected)"
Write-Host "Defender excluded:   $DefenderExcluded  (see docs/benchmarking-scans.md trap #1)"
Write-Host ""

function Invoke-ScanPhase([string]$phaseName, [string[]]$extraArgs) {
  # Deliberately does NOT redirect the native process's stderr into PowerShell's error stream
  # (`2>&1` on a native exe wraps every stderr line as a NativeCommandError and flips `$?` to
  # false even on exit code 0 under Windows PowerShell 5.1 -- see the PowerShell tool's own
  # documented caveat). The scan writes progress to stderr and its report to stdout; both are
  # let through to the console untouched, and only the exit code is checked.
  Write-Host "--- $phaseName ---" -ForegroundColor Yellow
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  & $BinaryPath scan $CorpusRoot @extraArgs
  $exitCode = $LASTEXITCODE
  $sw.Stop()
  Write-Host ""
  Write-Host ("[$phaseName] measured wall clock (process launch to exit): {0:N1} s" -f $sw.Elapsed.TotalSeconds) -ForegroundColor Green
  Write-Host ""
  if ($exitCode -ne 0) {
    throw "$phaseName failed with exit code $exitCode"
  }
}

# 1) `scan --force` on a brand-new catalog: the hashing path. Every file is new, so Phase::Hash
#    and Phase::DbWrite dominate; skip_check does essentially nothing on this pass.
Invoke-ScanPhase "scan --force (hashing path)" @('--force')

# 2) plain rescan immediately after, same catalog, same corpus, unchanged files: the skip_check
#    path. This is deliberately a *second* command invocation (not reusing the first process) so
#    both paths pay the same startup/config/snapshot overhead and only the per-file work differs.
Invoke-ScanPhase "scan (plain rescan, skip_check path)" @()

Write-Host "==================================================================" -ForegroundColor Cyan
Write-Host "Summary: $Label  (cache=$CacheCondition, defender_excluded=$DefenderExcluded)" -ForegroundColor Cyan
Write-Host "==================================================================" -ForegroundColor Cyan
Write-Host "Both phase breakdowns are printed above by the scan itself (see 'Where the time went')."
Write-Host "Do not compare 'scan --force' figures to plain-rescan figures -- different code paths."
Write-Host ""
Write-Host "Data dir used for this run (left in place for inspection, not auto-deleted): $dataDir"
