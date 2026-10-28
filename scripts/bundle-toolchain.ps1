# 组装可分发的 rip 工具链目录。
#
# 产物结构：
#   <Output>/rip.exe                       命令行入口（已编译的运行时）
#   <Output>/toolchain/bin/...             内置编译组件
#   <Output>/toolchain/lib/rustlib/<host>/ 目标平台的运行库与链接器
#   <Output>/lib/rip/deps/                 AOT 运行时库（librp_core-*.rlib 及其依赖）
#   <Output>/lib/rip/build/                依赖的构建脚本输出（原生库搜索路径）
#   <Output>/lib/rip/native/               依赖使用的外部原生库（如 winapi 导入库）
#
# 使用者拿到整个目录后即可执行：
#   rip run main.rp
#   rip build main.rp --aot
# 无需安装任何额外的编译工具。

param(
    [string]$RipExe = (Join-Path $PSScriptRoot "..\target\release\rip.exe"),
    [string]$AotDeps = (Join-Path $PSScriptRoot "..\target\aot\release\deps"),
    [string]$Output = (Join-Path $PSScriptRoot "..\dist\rip"),
    [string]$Rustc = "rustc"
)

$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    Write-Error $Message
    exit 1
}

if (-not (Test-Path $RipExe)) { Fail "找不到 rip 可执行文件：$RipExe（先执行 cargo build --release -p rp-cli）" }
if (-not (Test-Path $AotDeps)) { Fail "找不到 AOT 运行时库目录：$AotDeps（先执行 cargo build --release -p rp-core --target-dir target/aot）" }

# 统一成绝对路径：后续要判断构建记录里的路径是否位于 target 目录内。
$RipExe = (Resolve-Path $RipExe).Path
$AotDeps = (Resolve-Path $AotDeps).Path

$rlib = Get-ChildItem $AotDeps -Filter "librp_core-*.rlib" -File -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $rlib) { Fail "AOT 运行时库目录里没有 librp_core-*.rlib：$AotDeps" }

$sysroot = (& $Rustc --print sysroot).Trim()
if (-not (Test-Path $sysroot)) { Fail "无法定位编译器运行库目录：$sysroot" }
$hostTarget = (& $Rustc -vV | Select-String "^host:").ToString().Replace("host:", "").Trim()
$targetLib = Join-Path $sysroot "lib\rustlib\$hostTarget"
if (-not (Test-Path $targetLib)) { Fail "找不到目标平台运行库：$targetLib" }

Write-Host "rip     : $RipExe"
Write-Host "sysroot : $sysroot ($hostTarget)"
Write-Host "aot deps: $AotDeps"
Write-Host "output  : $Output"

if (Test-Path $Output) { Remove-Item $Output -Recurse -Force }
$toolchainBin = Join-Path $Output "toolchain\bin"
$toolchainLib = Join-Path $Output "toolchain\lib\rustlib\$hostTarget"
$ripDeps = Join-Path $Output "lib\rip\deps"
$ripBuild = Join-Path $Output "lib\rip\build"
foreach ($dir in @($Output, $toolchainBin, $toolchainLib, $ripDeps, $ripBuild)) {
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
}

# 1) 命令行入口
Copy-Item $RipExe (Join-Path $Output "rip.exe")

# 2) 内置编译组件：可执行文件 + 运行期 DLL
$sysBin = Join-Path $sysroot "bin"
$compilerFiles = @("rustc.exe", "libgcc_s_seh-1.dll", "libwinpthread-1.dll")
$compilerFiles += (Get-ChildItem $sysBin -Filter "rustc_driver-*.dll" -File | Select-Object -ExpandProperty Name)
$compilerFiles += (Get-ChildItem $sysBin -Filter "std-*.dll" -File | Select-Object -ExpandProperty Name)
foreach ($name in $compilerFiles) {
    $source = Join-Path $sysBin $name
    if (-not (Test-Path $source)) { Fail "编译组件缺失：$source" }
    Copy-Item $source (Join-Path $toolchainBin $name)
}

# 3) 目标平台运行库与链接器（含 self-contained 链接所需文件）
Copy-Item (Join-Path $targetLib "*") $toolchainLib -Recurse -Force

# 4) AOT 运行时库：rp_core 及其全部传递依赖
Copy-Item (Join-Path $AotDeps "*") $ripDeps -Recurse -Force
# 5) 依赖的构建脚本输出：原生库搜索路径与链接参数
$buildDir = Join-Path (Split-Path $AotDeps -Parent) "build"
if (Test-Path $buildDir) {
    Copy-Item (Join-Path $buildDir "*") $ripBuild -Recurse -Force
}

# 5b) 剔除链接阶段用不到的构建残留（白名单：只留 output 与静态库）：
#   - deps 下的 .rmeta（元数据已在 .rlib 内）与 .d（依赖记录，仅 cargo 增量用）
#   - build 下的构建脚本编译产物（build*script*build*）、依赖记录 .d、root-output、
#     stderr、invoked.timestamp、目标文件 .o、生成的 .rs/.expr，
#     以及 out/ 目录里除 .a/.lib 之外的一切
# 这些文件只对本机构建缓存有意义，随包带出既无用又会让目录里混入本机路径。
Remove-Item (Join-Path $ripDeps "*.rmeta"), (Join-Path $ripDeps "*.d") -Force -ErrorAction SilentlyContinue
Get-ChildItem -Path $ripBuild -Recurse -File -ErrorAction SilentlyContinue |
    Where-Object {
        $_.Extension -in @(".d", ".o", ".rs", ".expr") -or
        $_.Name -like "build*script*build*" -or
        $_.Name -in @("root-output", "stderr", "invoked.timestamp") -or
        ($_.Directory.Name -eq "out" -and $_.Extension -notin @(".a", ".lib"))
    } |
    Remove-Item -Force -ErrorAction SilentlyContinue
# 删掉因此变空的 out/ 与 crate 目录（由深到浅）。
Get-ChildItem -Path $ripBuild -Directory -Recurse -ErrorAction SilentlyContinue |
    Sort-Object { $_.FullName.Length } -Descending |
    ForEach-Object {
        if (-not (Get-ChildItem $_.FullName -Force -ErrorAction SilentlyContinue)) {
            Remove-Item $_.FullName -Force -ErrorAction SilentlyContinue
        }
    }

# 5c) 说明：编译器自带的 std 采用「.rlib 是 metadata stub + .rmeta 提供完整元数据」的分发形式，
#     两者的 .rmeta 与 rsbegin.o/rsend.o/crt2.o 都是编译链接输入，必须原样保留。

# 6) 外部原生库：从构建记录里找出不在 target 目录内的搜索路径（如 winapi 导入库）
$nativeDir = Join-Path $Output "lib\rip\native"
New-Item -ItemType Directory -Path $nativeDir -Force | Out-Null
$targetRoot = Split-Path (Split-Path $AotDeps -Parent) -Parent
$searches = @()
Get-ChildItem $ripBuild -Recurse -File -Filter "output" -ErrorAction SilentlyContinue | ForEach-Object {
    $searches += Get-Content $_.FullName | Where-Object { $_ -like "cargo:rustc-link-search=*" } | ForEach-Object {
        ($_ -replace '^cargo:rustc-link-search=', '' -replace '^native=', '').Trim()
    }
}
foreach ($path in ($searches | Select-Object -Unique)) {
    if ([string]::IsNullOrWhiteSpace($path)) { continue }
    if (-not (Test-Path $path)) { continue }
    $normalized = $path.Replace('/', '\')
    if ($normalized.StartsWith($targetRoot, [StringComparison]::OrdinalIgnoreCase)) { continue }
    $leaf = Split-Path $path -Leaf
    $crate = Split-Path (Split-Path $path -Parent) -Leaf
    $dest = Join-Path $nativeDir "$crate\$leaf"
    New-Item -ItemType Directory -Path $dest -Force | Out-Null
    Copy-Item (Join-Path $path "*") $dest -Recurse -Force
    Write-Host "native  : $path -> $dest"
}

# 7) 重写构建记录：只保留链接参数，并抹掉其中的本机绝对路径。
#    `output` 是 rip 编译 AOT 时读取链接参数的唯一来源，必须保留，但 rip 只读
#    四类行（rustc-link-search / rustc-link-arg[-bins] / rustc-link-lib），
#    其余行（rerun-if-changed、include、lib_dir、编译环境回显）只对 cargo 增量
#    构建有意义，且带有构建机绝对路径 —— 整段丢弃。
#    link-search 的绝对路径改写为 `<crate>\<leaf>` 两段相对形式，rip 会按目录名
#    映射到随包目录（localize_native_search：build/<crate>/out 或 native/<crate>/lib）。
$keepPrefixes = @(
    "cargo:rustc-link-search=",
    "cargo:rustc-link-arg=",
    "cargo:rustc-link-arg-bins=",
    "cargo:rustc-link-lib="
)
$strip = {
    param($match)
    $parts = $match.Groups[1].Value.Trim() -split '[\\/]'
    if ($parts.Count -ge 2) {
        "cargo:rustc-link-search=native=" + $parts[-2] + "\" + $parts[-1]
    } else {
        $match.Value
    }
}
$utf8 = New-Object System.Text.UTF8Encoding($false)
Get-ChildItem -Path $ripBuild -Recurse -File -Filter "output" -ErrorAction SilentlyContinue | ForEach-Object {
    $kept = @()
    foreach ($line in [IO.File]::ReadAllLines($_.FullName)) {
        if ($keepPrefixes | Where-Object { $line.StartsWith($_) }) {
            $kept += [regex]::Replace($line, 'cargo:rustc-link-search=native=([^\r\n]+)', $strip)
        }
    }
    if ($kept.Count -eq 0) {
        # 该依赖没有任何链接参数，整份记录无用，直接删除。
        Remove-Item $_.FullName -Force -ErrorAction SilentlyContinue
        return
    }
    [IO.File]::WriteAllText($_.FullName, (($kept -join "`n") + "`n"), $utf8)
}

# 7b) 重写后会留下没有链接参数的 crate 目录，删掉空目录（由深到浅）。
Get-ChildItem -Path $ripBuild -Directory -Recurse -ErrorAction SilentlyContinue |
    Sort-Object { $_.FullName.Length } -Descending |
    ForEach-Object {
        if (-not (Get-ChildItem $_.FullName -Force -ErrorAction SilentlyContinue)) {
            Remove-Item $_.FullName -Force -ErrorAction SilentlyContinue
        }
    }

# 8) 交付校验：全包范围内不允许残留构建机绝对路径或构建残留。
$bad = Get-ChildItem -Path (Join-Path $Output "lib") -Recurse -File -Filter "output" -ErrorAction SilentlyContinue |
    Where-Object { (Get-Content $_.FullName -Raw) -match '(^|[^A-Za-z])[A-Za-z]:[\\/]' }
$leftovers = Get-ChildItem -Path $Output -Recurse -File -ErrorAction SilentlyContinue |
    Where-Object {
        $_.Extension -in @(".d", ".rs", ".expr") -or
        $_.Name -like "build*script*build*" -or
        $_.Name -in @("root-output", "invoked.timestamp")
    }
# .rmeta / .o 只清理 AOT 运行时库一侧：编译器自带的 std .rmeta 与 rsend.o / crt2.o 都是编译链接输入。
$leftovers += Get-ChildItem -Path (Join-Path $Output "lib") -Recurse -File -ErrorAction SilentlyContinue |
    Where-Object { $_.Extension -in @(".rmeta", ".o") }
if ($bad -or $leftovers) {
    @($bad) + @($leftovers) | Where-Object { $_ } | ForEach-Object { Write-Host ("残留: " + $_.FullName) }
    Fail "交付目录不干净（见上）：仍有构建机路径或构建残留"
}

# 9) 安装器：双击 install.cmd 即把本目录写入当前用户 PATH（可逆）。
$installPs1 = @'
# Rip 运行时安装脚本（当前用户，无需管理员）
# 用法：powershell -ExecutionPolicy Bypass -File .\install.ps1 [-Uninstall]
param([switch]$Uninstall)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$marker = $root.TrimEnd("\") + "\"

function Get-UserPath {
    $v = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ([string]::IsNullOrWhiteSpace($v)) { return "" } else { return $v }
}

$current = Get-UserPath
$entries = @($current -split ";" | Where-Object { $_ -ne "" -and $_.TrimEnd("\") + "\" -ne $marker })

if ($Uninstall) {
    [Environment]::SetEnvironmentVariable("PATH", ($entries -join ";"), "User")
    Write-Host "已从用户 PATH 移除：$root"
    exit 0
}

# 校验安装目录完整（缺文件时给出明确提示，而不是让用户后面才遇到报错）。
foreach ($required in @("rip.exe", "toolchain", "lib\rip\deps")) {
    if (-not (Test-Path (Join-Path $root $required))) {
        Write-Error "安装目录不完整，缺少：$required（请重新解压完整安装包）"
        exit 1
    }
}

$entries += $root
[Environment]::SetEnvironmentVariable("PATH", (($entries | Select-Object -Unique) -join ";"), "User")
$env:PATH = $root + ";" + $env:PATH

Write-Host "已安装：$root"
Write-Host "已写入当前用户 PATH。新开一个终端后即可使用："
Write-Host "  rip version"
Write-Host "  rip new demo --template http"
Write-Host ""
Write-Host ("自检：& `"" + (Join-Path $root "rip.exe") + "`" doctor")
'@
Set-Content -Path (Join-Path $Output "install.ps1") -Value $installPs1 -Encoding UTF8

$installCmd = @'
@echo off
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %*
pause
'@
Set-Content -Path (Join-Path $Output "install.cmd") -Value $installCmd -Encoding ASCII

# 10) 使用说明：随包分发，安装后无需任何额外编译工具。
$readme = @"
# Rip 运行时（Windows x64）

自包含安装包：内置编译器、链接器与 AOT 运行时库，机器上无需安装 Rust / cargo / 任何编译工具。

[English](README.en.md) | 中文

## 安装

双击 `install.cmd`（或执行 `powershell -ExecutionPolicy Bypass -File .\install.ps1`），脚本会把本目录写入**当前用户 PATH**，无需管理员权限。
新开一个终端即可使用；卸载执行 `.\install.ps1 -Uninstall`。

手动方式（临时会话）：
```
. .\env.ps1        # 本目录内自带，等价于把当前目录加到 PATH
rip version
```

## 两种运行方式

```
rip new demo --template http     # 创建项目骨架
cd demo
rip run main.rp                  # 运行时 + 源码：解释执行（改源码即生效）
rip dev main.rp                  # 同上，带热重载
rip build main.rp -o dist        # AOT：编译为原生二进制，产物在 dist\ 与 dist.zip
```

## 目录结构

```
rip.exe                    命令行入口
install.cmd / install.ps1  安装器（写入用户 PATH）
toolchain\                 内置编译器（rustc）、目标平台运行库与链接器
lib\rip\deps\              AOT 运行时库（rp-core 及其依赖）
lib\rip\build\             依赖的链接参数（原生库搜索路径）
lib\rip\native\            依赖使用的外部原生库
```

## 环境变量

```
RIP_TOOLCHAIN   指定安装目录（默认取 rip.exe 所在目录）
RIP_AOT_DEPS    指定 AOT 运行时库目录（默认 lib\rip\deps）
```

## 自检

```
rip doctor
```
"@
Set-Content -Path (Join-Path $Output "README.md") -Value $readme -Encoding UTF8

$readmeEn = @"
# Rip Runtime (Windows x64)

Self-contained distribution: the compiler, linker and AOT runtime libraries are bundled,
so no Rust / cargo / build tools are required on the target machine.

English | [中文](README.md)

## Install

Run `install.cmd` (or `powershell -ExecutionPolicy Bypass -File .\install.ps1`). The script appends
this directory to the **current user PATH**; administrator rights are not required.
Open a new terminal afterwards. Uninstall with `.\install.ps1 -Uninstall`.

Manual setup for the current session:
```
. .\env.ps1        # ships with the package; prepends this directory to PATH
rip version
```

## Two ways to run

```
rip new demo --template http     # scaffold a project
cd demo
rip run main.rp                  # runtime + source: interpreted, edits take effect immediately
rip dev main.rp                  # same, with hot reload
rip build main.rp -o dist        # AOT: native binary in dist\ plus dist.zip
```

## Layout

```
rip.exe                    command line entry point
install.cmd / install.ps1  installer (writes user PATH)
toolchain\                 bundled rustc, target libraries and linker
lib\rip\deps\              AOT runtime library (rp-core and its dependencies)
lib\rip\build\             link parameters of dependencies (native search paths)
lib\rip\native\            external native libraries used by dependencies
```

## Environment variables

```
RIP_TOOLCHAIN   installation directory (defaults to the directory of rip.exe)
RIP_AOT_DEPS    AOT runtime library directory (defaults to lib\rip\deps)
```

## Self check

```
rip doctor
```
"@
Set-Content -Path (Join-Path $Output "README.en.md") -Value $readmeEn -Encoding UTF8

# env.ps1：手动会话用的极简环境脚本（不修改系统设置）。
$envPs1 = @'
# 把 Rip 安装目录加入当前会话 PATH（不改系统设置）。
$ripRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
if (($env:PATH -split ";") -notcontains $ripRoot) { $env:PATH = $ripRoot + ";" + $env:PATH }
Write-Host "Rip 已就绪（当前会话）：$ripRoot"
'@
Set-Content -Path (Join-Path $Output "env.ps1") -Value $envPs1 -Encoding UTF8

$total = (Get-ChildItem $Output -Recurse -File | Measure-Object -Property Length -Sum).Sum / 1MB
Write-Host ("完成：{0}（{1:N0} MB）" -f $Output, $total)
Write-Host "冒烟验证："
Write-Host "  & `"$Output\rip.exe`" build <项目>\main.rp --aot -o <输出目录>"



