$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Write-ErrorAndExit {
    param([Parameter(Mandatory = $true)][string]$Message)
    Write-Host "Error: $Message" -ForegroundColor Red
    exit 1
}

function Write-Success {
    param([Parameter(Mandatory = $true)][string]$Message)
    Write-Host $Message -ForegroundColor Green
}

function Write-Warn {
    param([Parameter(Mandatory = $true)][string]$Message)
    Write-Host $Message -ForegroundColor Yellow
}

$installDir = Join-Path $HOME '.git-ai\bin'
$localDevGitwrapDir = Join-Path $HOME '.git-ai-local-dev\gitwrap\bin'
$gitAiBinary = Join-Path $installDir 'git-ai.exe'
$gitAiDir = Join-Path $HOME '.git-ai'
$gitAiLocalDevDir = Join-Path $HOME '.git-ai-local-dev'
$scriptDir = $PSScriptRoot
if (-not $scriptDir) {
    $scriptDir = (Get-Location).Path
}
$gitAiBinaryCandidates = @(
    $gitAiBinary,
    (Join-Path $localDevGitwrapDir 'git-ai.exe'),
    (Join-Path $scriptDir 'target\debug\git-ai.exe'),
    (Join-Path $scriptDir 'target\release\git-ai.exe')
)

function Resolve-GitAiBinary {
    foreach ($candidate in $gitAiBinaryCandidates) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return $candidate
        }
    }

    try {
        $pathCmd = Get-Command git-ai -ErrorAction Stop
        if ($pathCmd.Source -and (Test-Path -LiteralPath $pathCmd.Source -PathType Leaf)) {
            return $pathCmd.Source
        }
    } catch {
    }

    return $null
}

Write-Host 'Uninstalling git-ai...'
Write-Host ''

# ============================================================
# Step 1: Run uninstall-hooks while the binary is still present.
# This removes IDE/agent hooks, skills, and git client prefs.
# Must happen before we delete the binary in step 4.
# ============================================================
$uninstallHooksBinary = Resolve-GitAiBinary
if ($uninstallHooksBinary) {
    Write-Host 'Removing IDE/agent hooks...'
    try {
        & $uninstallHooksBinary uninstall-hooks '--dry-run=false' | Out-Host
        Write-Success 'IDE/agent hooks removed.'
    } catch {
        Write-Warn "uninstall-hooks reported errors. Continuing with remaining steps."
    }
} else {
    Write-Host "git-ai binary not found in install or dev locations — skipping uninstall-hooks."
}

Write-Host ''

# ============================================================
# Step 2: Remove install directory from User and Machine PATH.
# The installer adds $installDir to both scopes via
# [Environment]::SetEnvironmentVariable.
# ============================================================
function Remove-FromPath {
    param(
        [Parameter(Mandatory = $true)][string]$PathToRemove,
        [Parameter(Mandatory = $true)][string]$Scope
    )
    try {
        $current = [Environment]::GetEnvironmentVariable('Path', $Scope)
        if (-not $current) { return 'NotPresent' }

        $normalized = ([IO.Path]::GetFullPath($PathToRemove.Trim())).TrimEnd('\').ToLowerInvariant()
        $entries = ($current -split ';') | Where-Object { $_ -and $_.Trim() -ne '' }
        $filtered = $entries | Where-Object {
            try { ([IO.Path]::GetFullPath($_.Trim())).TrimEnd('\').ToLowerInvariant() -ne $normalized }
            catch { $true }
        }

        $newPath = ($filtered -join ';')
        if ($newPath -eq $current) { return 'NotPresent' }

        [Environment]::SetEnvironmentVariable('Path', $newPath, $Scope)
        return 'Removed'
    } catch {
        return 'Error'
    }
}

$pathsToRemove = @($installDir, $localDevGitwrapDir)
foreach ($scope in @('User', 'Machine')) {
    foreach ($pathEntry in $pathsToRemove) {
        $result = Remove-FromPath -PathToRemove $pathEntry -Scope $scope
        switch ($result) {
            'Removed' {
                Write-Success "Removed $pathEntry from $scope PATH."
            }
            'NotPresent' {
                Write-Host "$scope PATH: $pathEntry not present — nothing to remove."
            }
            'Error' {
                if ($scope -eq 'Machine') {
                    Write-Warn "Could not update $scope PATH (administrator rights may be required). Remove $pathEntry manually if needed."
                } else {
                    Write-Warn "Could not update $scope PATH. You may need to remove $pathEntry manually."
                }
            }
        }
    }
}

# Remove from current process PATH as well so this session is immediately clean.
try {
    $normalizedPaths = @()
    foreach ($pathEntry in $pathsToRemove) {
        $normalizedPaths += ([IO.Path]::GetFullPath($pathEntry.Trim())).TrimEnd('\').ToLowerInvariant()
    }

    $procEntries = ($env:PATH -split ';') | Where-Object {
        if (-not $_ -or $_.Trim() -eq '') { return $false }
        try {
            $entryNormalized = ([IO.Path]::GetFullPath($_.Trim())).TrimEnd('\').ToLowerInvariant()
            return -not ($normalizedPaths -contains $entryNormalized)
        } catch {
            return $true
        }
    }
    $env:PATH = ($procEntries -join ';')
} catch { }

Write-Host ''

# ============================================================
# Step 2b: Strip PATH entries from Git Bash shell configs.
# The Windows installer writes ".git-ai/bin" entries; local-dev
# setup may also write "git-ai-local-dev/gitwrap/bin" or
# "target/gitwrap/bin" entries (with slash or backslash variants).
# ============================================================
$cleanupRegexes = @(
    [regex]::Escape('.git-ai/bin'),
    [regex]::Escape('.git-ai\bin'),
    [regex]::Escape('git-ai-local-dev/gitwrap/bin'),
    [regex]::Escape('git-ai-local-dev\gitwrap\bin'),
    [regex]::Escape('target/gitwrap/bin'),
    [regex]::Escape('target\gitwrap\bin')
)
$bashConfigs = @(
    (Join-Path $HOME '.bashrc'),
    (Join-Path $HOME '.bash_profile')
)
$bashCleaned = @()

foreach ($configFile in $bashConfigs) {
    if (-not (Test-Path -LiteralPath $configFile)) { continue }
    try {
        $content = Get-Content -LiteralPath $configFile -Raw -ErrorAction SilentlyContinue
        $hasCleanupTargets = $false
        if ($content) {
            foreach ($rx in $cleanupRegexes) {
                if ($content -match $rx) {
                    $hasCleanupTargets = $true
                    break
                }
            }
        }

        if ($hasCleanupTargets) {
            $lines = Get-Content -LiteralPath $configFile -Encoding UTF8
            $filtered = foreach ($line in $lines) {
                $removeLine = $line -match '# Added by git-ai installer|# git-ai local dev'
                if (-not $removeLine) {
                    foreach ($rx in $cleanupRegexes) {
                        if ($line -match $rx) {
                            $removeLine = $true
                            break
                        }
                    }
                }

                if (-not $removeLine) {
                    $line
                }
            }
            $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
            [System.IO.File]::WriteAllLines($configFile, $filtered, $utf8NoBom)
            $bashCleaned += $configFile
        }
    } catch {
        Write-Warn "Could not update $configFile : $($_.Exception.Message)"
    }
}

if ($bashCleaned.Count -gt 0) {
    Write-Host 'Removed PATH entries from Git Bash config(s):'
    foreach ($f in $bashCleaned) { Write-Host "  ✓ $f" }
    Write-Host ''
}

# ============================================================
# Step 3: Remove the ~/.local/bin/git-ai symlink/file.
# ============================================================
$localBin = Join-Path $HOME '.local\bin\git-ai'
if (Test-Path -LiteralPath $localBin) {
    Remove-Item -Force -LiteralPath $localBin -ErrorAction SilentlyContinue
    Write-Success "Removed $localBin"
}

# ============================================================
# Step 4: Remove ~/.git-ai/ — contains the binary, git and
# git-og shims, config.json, and internal state.
# ============================================================
if (Test-Path -LiteralPath $gitAiDir) {
    Remove-Item -Recurse -Force -LiteralPath $gitAiDir
    Write-Success "Removed $gitAiDir"
} else {
    Write-Host "$gitAiDir not found — nothing to remove."
}

# Remove local-dev install directory if present.
if (Test-Path -LiteralPath $gitAiLocalDevDir) {
    Remove-Item -Recurse -Force -LiteralPath $gitAiLocalDevDir
    Write-Success "Removed $gitAiLocalDevDir"
} else {
    Write-Host "$gitAiLocalDevDir not found — nothing to remove."
}

# ============================================================
# Step 5: Remove stale git.path overrides in editor settings
# when they still point to git-ai wrapper paths.
# ============================================================
$editorSettingsFiles = @(
    (Join-Path $HOME 'Library\Application Support\Cursor\User\settings.json'),
    (Join-Path $HOME 'Library\Application Support\Code\User\settings.json'),
    (Join-Path $HOME 'Library\Application Support\Code - Insiders\User\settings.json'),
    (Join-Path $HOME 'AppData\Roaming\Cursor\User\settings.json'),
    (Join-Path $HOME 'AppData\Roaming\Code\User\settings.json'),
    (Join-Path $HOME 'AppData\Roaming\Code - Insiders\User\settings.json')
)

foreach ($settingsFile in $editorSettingsFiles) {
    if (-not (Test-Path -LiteralPath $settingsFile)) { continue }
    try {
        $raw = Get-Content -LiteralPath $settingsFile -Raw -ErrorAction Stop
        $json = $raw | ConvertFrom-Json -ErrorAction Stop
        $gitPathValue = $json.'git.path'

        if ($gitPathValue -is [string] -and (
            $gitPathValue -match '\.git-ai' -or
            $gitPathValue -match 'git-ai-local-dev' -or
            $gitPathValue -match 'gitwrap[\\/]+bin[\\/]+git'
        )) {
            $json.PSObject.Properties.Remove('git.path') | Out-Null
            $json | ConvertTo-Json -Depth 100 | Set-Content -LiteralPath $settingsFile -Encoding UTF8
            Write-Success "Removed stale git.path from $settingsFile"
        }
    } catch {
        Write-Warn "Could not update $settingsFile : $($_.Exception.Message)"
    }
}

Write-Host ''
Write-Success 'git-ai has been uninstalled.'
Write-Host ''
Write-Host 'Next steps:'
Write-Host '  • Open a new terminal session to apply PATH changes.'
Write-Host '  • If you installed the VS Code or Cursor extension, uninstall it via the Extensions panel.'
Write-Host '  • If you installed the JetBrains plugin, uninstall via Settings > Plugins > git-ai > Uninstall.'
