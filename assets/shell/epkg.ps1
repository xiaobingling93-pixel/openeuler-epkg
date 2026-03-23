# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.
#
# Integration model (why the profile filename looks "Microsoft.*"):
# - PowerShell only auto-loads profiles at fixed paths. The usual current-user file is
#   Microsoft.PowerShell_profile.ps1 under ~/.config/powershell/ (pwsh on Linux/macOS) or
#   %USERPROFILE%/Documents/PowerShell/ (pwsh on Windows). "Microsoft.PowerShell" is the host
#   name of the default console host, not a random label; epkg did not invent that name.
# - `epkg self install` / light_init append a small # epkg begin/end block to that file;
#   the block dot-sources this script. Real logic and the `epkg` function live here.
#
# Dot-source manually if needed:
#   . "$HOME/.epkg/envs/self/usr/src/epkg/assets/shell/epkg.ps1"

function Get-EpkgSelfEnvRoot {
    $candidates = [System.Collections.ArrayList]@()
    if ($env:USERPROFILE) {
        [void]$candidates.Add((Join-Path $env:USERPROFILE '.epkg/envs/self'))
    }
    [void]$candidates.Add((Join-Path $HOME '.epkg/envs/self'))
    [void]$candidates.Add('C:\epkg\envs\root\self')
    [void]$candidates.Add('/opt/epkg/envs/root/self')
    foreach ($c in $candidates) {
        if ($c -and (Test-Path -LiteralPath $c)) {
            return $c
        }
    }
    return $null
}

function Get-EpkgRustPath {
    param([string]$SelfEnvRoot)
    foreach ($p in @(
            (Join-Path $SelfEnvRoot 'usr/bin/epkg'),
            (Join-Path $SelfEnvRoot 'usr\bin\epkg.exe')
        )) {
        if (Test-Path -LiteralPath $p) {
            return $p
        }
    }
    return $null
}

function epkg {
    [CmdletBinding()]
    param([Parameter(ValueFromRemainingArguments)][string[]]$ArgumentList)

    $envSelfDir = Get-EpkgSelfEnvRoot
    if (-not $envSelfDir) {
        Write-Error 'epkg: self environment not found'
        return
    }

    $epkgRust = Get-EpkgRustPath -SelfEnvRoot $envSelfDir
    if (-not $epkgRust) {
        Write-Error 'epkg: command not found'
        return
    }

    if ($env:EPKG_ACTIVE_ENV) {
        $bad = $false
        foreach ($n in $env:EPKG_ACTIVE_ENV -split ':') {
            $nn = $n.TrimEnd('!')
            $p = Join-Path $HOME ".epkg/envs/$nn"
            if (-not (Test-Path -LiteralPath $p)) {
                $bad = $true
                break
            }
        }
        if ($bad) {
            Remove-Item Env:EPKG_ACTIVE_ENV -ErrorAction SilentlyContinue
        }
    }

    $cmd = ''
    $subCmd = ''
    $i = 0
    $skipNext = $false
    $hasHelp = $false
    while ($i -lt $ArgumentList.Count) {
        $arg = $ArgumentList[$i]
        if ($skipNext) {
            $skipNext = $false
            $i++
            continue
        }
        switch -Regex ($arg) {
            '^--$' { break }
            '^--[^=]+=.+$' { }
            '^(-h|--help|-V|--version|-q|--quiet|-v|--verbose|-y|--assume-yes|--dry-run|--download-only|--assume-no|-m|--ignore-missing)$' {
                $hasHelp = $true
            }
            '^(-e|--env|-r|--root|--config|--arch|--metadata-expire|--proxy|--retry|--parallel-download|--parallel-processing)$' {
                $skipNext = $true
            }
            '^-' {
                $skipNext = $true
            }
            default {
                if (-not $cmd) {
                    $cmd = $arg
                }
                elseif (-not $subCmd) {
                    $subCmd = $arg
                }
            }
        }
        $i++
    }

    $env:EPKG_SHELL = 'powershell'

    if ($cmd -eq 'env' -and $subCmd -in @('path', 'register', 'unregister', 'activate', 'deactivate', 'remove')) {
        $out = & $epkgRust @ArgumentList 2>&1 | ForEach-Object { $_.ToString() }
        if ($LASTEXITCODE -ne 0) {
            $out | Write-Host
            return
        }
        $out | Write-Host
        if (-not $hasHelp) {
            $text = $out -join "`n"
            Invoke-Expression $text
            __EpkgRehashPath
        }
        return
    }

    if ($cmd -in @('install', 'remove', 'switch')) {
        & $epkgRust @ArgumentList
        if ($LASTEXITCODE -eq 0) {
            __EpkgRehashPath
        }
        return
    }

    & $epkgRust @ArgumentList
}

function __EpkgRehashPath {
}

$__epkgSelf = Get-EpkgSelfEnvRoot
$__epkgRust = if ($__epkgSelf) { Get-EpkgRustPath -SelfEnvRoot $__epkgSelf } else { $null }
if ($__epkgRust) {
    $env:EPKG_SHELL = 'powershell'
    $out = & $__epkgRust env path 2>&1 | ForEach-Object { $_.ToString() }
    if ($LASTEXITCODE -eq 0) {
        Invoke-Expression ($out -join "`n")
    }
}
