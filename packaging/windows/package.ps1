# 组装 Windows 分发件（便携 zip + 可选 Inno Setup 安装器）。
#
# 用法（在仓库根目录）：
#   pwsh -File packaging/windows/package.ps1 -Version 0.6.1
#
# 布局必须长这样——`crates/miyu-base/src/paths/resources.rs` 里
# `installation_prefix = exe.parent().parent()`，资源去 `<prefix>/share/miyu/<kind>` 找：
#
#   <安装目录>\bin\miyu.exe
#   <安装目录>\bin\rg.exe                      ← 文件搜索工具直接 spawn `rg`，走 PATH
#   <安装目录>\share\miyu\fonts\*.ttf|ttc      ← 渲染器硬要求 NotoSansCJK-Regular.ttc
#   <安装目录>\share\miyu\memes\...
#   <安装目录>\share\miyu\scripts\...
#   <安装目录>\share\licenses\miyu\*
#
# 所以 `bin/` 与 `share/` 两层不能省，zip 解压到任意目录即可用。

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $Version,

    [string] $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,
    [string] $OutDir = '',
    [string] $RipgrepVersion = '14.1.1',
    [string] $RipgrepPath = '',
    [switch] $SkipRipgrep,
    [switch] $SkipInstaller
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $OutDir) { $OutDir = Join-Path $RepoRoot 'out\windows' }

function Write-Step([string] $Message) { Write-Host "==> $Message" -ForegroundColor Cyan }
function Write-Note([string] $Message) { Write-Host "    $Message" -ForegroundColor DarkGray }

# ---------------------------------------------------------------------------
# 0. 前置检查
# ---------------------------------------------------------------------------

$manifest = Join-Path $RepoRoot 'Cargo.toml'
if (-not (Test-Path $manifest)) { throw "RepoRoot 不像仓库根（缺 Cargo.toml）：$RepoRoot" }

$binary = Join-Path $RepoRoot 'target\release\miyu.exe'
if (-not (Test-Path $binary)) {
    throw "找不到 $binary——先跑 `cargo build --release`（或 `cargo build --release --features voice`）。"
}

# 版本号必须与二进制自报的一致，否则就是发错标签的包。
$reported = (& $binary --version 2>&1 | Select-Object -First 1)
if ($reported -notmatch [regex]::Escape($Version)) {
    throw "二进制自报 `$reported`，与 -Version $Version 不符；先确认构建产物是这一版。"
}
Write-Step "已确认 $binary 自报 $reported"

$stage = Join-Path $OutDir 'miyu-windows-x64'
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }

$binDir = Join-Path $stage 'bin'
$shareMiyu = Join-Path $stage 'share\miyu'
$fontDir = Join-Path $shareMiyu 'fonts'
$licenseDir = Join-Path $stage 'share\licenses\miyu'
foreach ($dir in @($binDir, $fontDir, $licenseDir, (Join-Path $shareMiyu 'memes'), (Join-Path $shareMiyu 'scripts'))) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
}

# ---------------------------------------------------------------------------
# 1. 二进制与资源
# ---------------------------------------------------------------------------

Write-Step '放二进制'
Copy-Item $binary (Join-Path $binDir 'miyu.exe') -Force
$voice = Join-Path $RepoRoot 'target\release\miyu-voice.exe'
if (Test-Path $voice) {
    Copy-Item $voice (Join-Path $binDir 'miyu-voice.exe') -Force
    Write-Note '带上 miyu-voice.exe（voice 特性已编）'
} else {
    Write-Note '未编 voice 特性，不含 miyu-voice.exe（与 README 里「语音组件暂无 Windows 发行包」一致）'
}

Write-Step '放字体'
# 渲染器（crates/miyu-hosts/.../renderer/fonts.rs）按文件名认这三份：
# CJK 是硬要求（缺了直接报错），另外两份是代码体与 emoji 回退。
foreach ($font in @('NotoSansCJK-Regular.ttc', 'JetBrainsMono-Regular.ttf', 'NotoColorEmoji.ttf')) {
    $src = Join-Path $RepoRoot "assets\fonts\$font"
    if (-not (Test-Path $src)) { throw "缺字体 $src" }
    Copy-Item $src (Join-Path $fontDir $font) -Force
    Write-Note $font
}

Write-Step '放 memes 与 scripts'
Copy-Item (Join-Path $RepoRoot 'src\memes\*') (Join-Path $shareMiyu 'memes') -Recurse -Force
Copy-Item (Join-Path $RepoRoot 'src\scripts\*') (Join-Path $shareMiyu 'scripts') -Recurse -Force
Write-Note ("memes {0} 项 / scripts {1} 项" -f (Get-ChildItem (Join-Path $shareMiyu 'memes') -Recurse -File).Count,
                                          (Get-ChildItem (Join-Path $shareMiyu 'scripts') -Recurse -File).Count)

Write-Step '放许可证'
Copy-Item (Join-Path $RepoRoot 'LICENSE') (Join-Path $licenseDir 'LICENSE') -Force
foreach ($lic in @('NotoSansCJK.LICENSE', 'JetBrainsMono.LICENSE', 'NotoColorEmoji.LICENSE')) {
    Copy-Item (Join-Path $RepoRoot "assets\fonts\$lic") (Join-Path $licenseDir $lic) -Force
}

# ---------------------------------------------------------------------------
# 2. ripgrep（文件搜索工具直接 spawn `rg`，缺了那几件工具直接报错）
# ---------------------------------------------------------------------------

if ($SkipRipgrep) {
    Write-Step '跳过 ripgrep（-SkipRipgrep）'
} elseif ($RipgrepPath) {
    if (-not (Test-Path $RipgrepPath)) { throw "找不到 -RipgrepPath $RipgrepPath" }
    Write-Step "用本地 ripgrep：$RipgrepPath"
    Copy-Item $RipgrepPath (Join-Path $binDir 'rg.exe') -Force
} else {
    Write-Step "下载 ripgrep $RipgrepVersion"
    $asset = "ripgrep-$RipgrepVersion-x86_64-pc-windows-msvc.zip"
    $base = "https://github.com/BurntSushi/ripgrep/releases/download/$RipgrepVersion"
    $tmp = Join-Path $OutDir 'ripgrep'
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    $zip = Join-Path $tmp $asset

    try {
        Invoke-WebRequest -Uri "$base/$asset" -OutFile $zip
    } catch {
        throw "下载 ripgrep 失败（$base/$asset）：$($_.Exception.Message)`n换个 -RipgrepVersion，或用 -RipgrepPath 指本地 rg.exe。"
    }
    # 同一次 release 里发布的 .sha256；对不上就停，别把来源不明的东西打进包里。
    # 拿不到校验文件同样停：宁可让人显式换版本，也不默默跳过校验。
    #
    # 走 -OutFile 再读文件，别直接取 .Content：PowerShell 7 的 Invoke-WebRequest 对
    # 非文本 Content-Type（.sha256 这类）返回的是 byte[]，`.Content.Trim()` 会变成
    # 逐字节调用 Trim，报「[System.Byte] does not contain a method named 'Trim'」——
    # CI 上就是这么挂的（2026-09-23 首次跑 Windows package）。
    $shaFile = Join-Path $tmp "$asset.sha256"
    try {
        Invoke-WebRequest -Uri "$base/$asset.sha256" -OutFile $shaFile
    } catch {
        throw "拿不到 $asset.sha256（这个版本可能没发校验文件）：$($_.Exception.Message)`n换个 -RipgrepVersion，或用 -RipgrepPath 指本地 rg.exe。"
    }
    # 去掉可能的 BOM：.NET 的 Trim() 不认 U+FEFF。
    #
    # 校验文件的格式各家不统一：GNU 的 `<hash>  <file>`、BSD 的
    # `SHA256 (file) = <hash>`、或者一份含全部资产的 SHA256SUMS。所以别假设格式，
    # 按「行里含 64 位十六进制」取，并优先挑点名了本资产的那一行。
    # （ripgrep 14.1.1 就是 BSD 风格——2026-09-23 CI 上按两空格解析拿到的是 "SHA256"。）
    $shaText = (Get-Content $shaFile -Raw) -replace "^\uFEFF", ""
    $candidates = @($shaText -split "\r?\n" | Where-Object { $_ -match '[0-9a-fA-F]{64}' })
    $line = @($candidates | Where-Object { $_ -match [regex]::Escape($asset) } | Select-Object -First 1)
    if ($line.Count -eq 0) { $line = @($candidates | Select-Object -First 1) }
    if ($line.Count -eq 0) {
        throw "校验文件里找不到 64 位 sha256，原文是：$($shaText.Trim())`n换个 -RipgrepVersion，或用 -RipgrepPath 指本地 rg.exe。"
    }
    $expected = [regex]::Match($line[0], '[0-9a-fA-F]{64}').Value
    $actual = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $expected.ToLower()) {
        throw "ripgrep 校验失败：期望 $expected，实际 $actual"
    }
    Write-Note "sha256 $actual 已校验"

    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    $rg = Get-ChildItem $tmp -Recurse -Filter 'rg.exe' | Select-Object -First 1
    if (-not $rg) { throw "压缩包里没有 rg.exe：$zip" }
    Copy-Item $rg.FullName (Join-Path $binDir 'rg.exe') -Force
}

# ---------------------------------------------------------------------------
# 3. 便携 zip
# ---------------------------------------------------------------------------

Write-Step '打便携 zip'
$zipName = "Miyu-$Version-windows-x64.zip"
$zipPath = Join-Path $OutDir $zipName
if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
Add-Type -AssemblyName System.IO.Compression.FileSystem
# 用 .NET 而不是 Compress-Archive：一百多 MB 下前者快一个数量级。
[System.IO.Compression.ZipFile]::CreateFromDirectory(
    $stage, $zipPath, [System.IO.Compression.CompressionLevel]::Optimal, $false)
$zipMb = [math]::Round((Get-Item $zipPath).Length / 1MB, 1)
Write-Note "$zipName（$zipMb MB）"

# ---------------------------------------------------------------------------
# 4. Inno Setup 安装器（可选：本机没装 ISCC 就跳过）
# ---------------------------------------------------------------------------

if ($SkipInstaller) {
    Write-Step '跳过安装器（-SkipInstaller）'
} else {
    $isccExe = $Iscc
    if (-not $isccExe) {
        foreach ($candidate in @(
                (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
                (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'))) {
            if ($candidate -and (Test-Path $candidate)) { $isccExe = $candidate; break }
        }
    }
    if (-not $isccExe -or -not (Test-Path $isccExe)) {
        Write-Step '没找到 Inno Setup 的 ISCC.exe，跳过安装器'
        Write-Note '装上 Inno Setup 6 后重跑，或用 -Iscc <路径> 指过去；CI 里 runner 自带。'
    } else {
        Write-Step "打 Inno Setup 安装器（$isccExe）"
        $iss = Join-Path $PSScriptRoot 'miyu.iss'
        $outExe = Join-Path $OutDir "MiyuSetup-$Version-x64.exe"
        & $isccExe "/DAppVersion=$Version" "/DStageDir=$stage" "/DOutDir=$OutDir" $iss | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "ISCC 失败（exit $LASTEXITCODE）" }
        if (-not (Test-Path $outExe)) { throw "ISCC 跑完但没有 $outExe" }
        $exeMb = [math]::Round((Get-Item $outExe).Length / 1MB, 1)
        Write-Note "MiyuSetup-$Version-x64.exe（$exeMb MB）"
    }
}

Write-Step '完成'
Write-Note "输出目录：$OutDir"
Get-ChildItem $OutDir -File | ForEach-Object { Write-Note ("{0}  {1} MB" -f $_.Name, [math]::Round($_.Length / 1MB, 1)) }
