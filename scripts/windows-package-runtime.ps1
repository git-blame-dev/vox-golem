param(
    [ValidateSet('StageRelease', 'PackageDist')]
    [string]$Mode = 'StageRelease',
    [string]$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
)

$ErrorActionPreference = 'Stop'

$repoRootPath = [System.IO.Path]::GetFullPath($RepoRoot)
$releaseDir = Join-Path $repoRootPath 'target\release'
$distDir = Join-Path $repoRootPath 'dist\VoxGolem'
$configTemplate = Join-Path $repoRootPath 'config.example.toml'

$requiredCudaDlls = @(
    'cublasLt64_12.dll',
    'cublas64_12.dll',
    'cufft64_11.dll',
    'cudart64_12.dll',
    'cudnn64_9.dll',
    'cudnn_adv64_9.dll',
    'cudnn_cnn64_9.dll',
    'cudnn_engines_precompiled64_9.dll',
    'cudnn_engines_runtime_compiled64_9.dll',
    'cudnn_graph64_9.dll',
    'cudnn_heuristic64_9.dll',
    'cudnn_ops64_9.dll'
)

$requiredReleaseFiles = @(
    'vox-golem.exe',
    'onnxruntime_providers_cuda.dll',
    'onnxruntime_providers_shared.dll'
) + $requiredCudaDlls

function Assert-FilesExist {
    param(
        [string]$Directory,
        [string[]]$FileNames,
        [string]$Description
    )

    $missingFiles = @(
        $FileNames | Where-Object {
            -not (Test-Path -LiteralPath (Join-Path $Directory $_) -PathType Leaf)
        }
    )

    if ($missingFiles.Count -gt 0) {
        throw ('Missing required {0} files in {1}: {2}' -f $Description, $Directory, ($missingFiles -join ', '))
    }
}

function Write-FileListing {
    param(
        [string]$Directory,
        [string]$Header
    )

    Write-Host $Header
    Get-ChildItem -LiteralPath $Directory -File |
        Sort-Object Name |
        ForEach-Object { Write-Host ("  {0}`t{1}" -f $_.Name, $_.Length) }
}

function Stage-ReleaseRuntime {
    if ([string]::IsNullOrWhiteSpace($env:VOXGOLEM_CUDA_RUNTIME_DIR)) {
        throw 'VOXGOLEM_CUDA_RUNTIME_DIR must be set to the directory that contains the CUDA runtime DLLs.'
    }

    $runtimeDir = [System.IO.Path]::GetFullPath($env:VOXGOLEM_CUDA_RUNTIME_DIR)

    if (-not (Test-Path -LiteralPath $runtimeDir -PathType Container)) {
        throw "VOXGOLEM_CUDA_RUNTIME_DIR does not exist or is not a directory: $runtimeDir"
    }

    if (-not (Test-Path -LiteralPath $releaseDir -PathType Container)) {
        throw "Release directory does not exist: $releaseDir"
    }

    Assert-FilesExist -Directory $runtimeDir -FileNames $requiredCudaDlls -Description 'CUDA runtime source'

    Write-Host "[windows-runtime] staging CUDA runtime DLLs from: $runtimeDir"
    Write-Host "[windows-runtime] staging runtime DLLs to: $releaseDir"

    $runtimeDllsByName = @{}
    foreach ($dllName in $requiredCudaDlls) {
        $sourcePath = Join-Path $runtimeDir $dllName
        $runtimeDllsByName[$dllName] = Get-Item -LiteralPath $sourcePath
    }

    foreach ($dllName in ($runtimeDllsByName.Keys | Sort-Object)) {
        $sourceFile = $runtimeDllsByName[$dllName]
        Copy-Item -LiteralPath $sourceFile.FullName -Destination (Join-Path $releaseDir $sourceFile.Name) -Force
        Write-Host "[windows-runtime] copied $($sourceFile.Name)"
    }

    Assert-FilesExist -Directory $releaseDir -FileNames $requiredReleaseFiles -Description 'release runtime'
    Write-FileListing -Directory $releaseDir -Header '[windows-runtime] release directory files after staging:'
    Write-Host '[windows-runtime] release runtime staging complete.'
}

function Package-DistRuntime {
    Stage-ReleaseRuntime

    if (-not (Test-Path -LiteralPath $configTemplate -PathType Leaf)) {
        throw "Config template was not found: $configTemplate"
    }

    if (Test-Path -LiteralPath $distDir) {
        Remove-Item -LiteralPath $distDir -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $distDir | Out-Null

    Copy-Item -LiteralPath (Join-Path $releaseDir 'vox-golem.exe') -Destination (Join-Path $distDir 'vox-golem.exe') -Force
    Copy-Item -LiteralPath $configTemplate -Destination (Join-Path $distDir 'config.toml') -Force
    Get-ChildItem -LiteralPath $releaseDir -Filter '*.dll' -File |
        ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $distDir $_.Name) -Force }

    Assert-FilesExist -Directory $distDir -FileNames (@('config.toml') + $requiredReleaseFiles) -Description 'packaged app'
    Write-FileListing -Directory $distDir -Header '[windows-runtime] packaged app files:'
    Write-Host '[windows-runtime] packaged app staging complete.'
}

switch ($Mode) {
    'StageRelease' { Stage-ReleaseRuntime }
    'PackageDist' { Package-DistRuntime }
}
