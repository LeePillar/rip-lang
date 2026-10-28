# rip 命令包装脚本：自动定位本仓库编译出的 rip.exe，并临时加入 PATH。
# 用法（在 demo 目录下）：
#   .\rip.ps1 version
#   .\rip.ps1 check server
#   .\rip.ps1 server --port 8080      # 等价于 cd server; rip run main.rp --port 8080
#
# 若提示禁止运行脚本，用：
#   powershell -ExecutionPolicy Bypass -File .\rip.ps1 version

$ErrorActionPreference = "Stop"

$demo = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = Split-Path -Parent $demo
$exe = @(
    (Join-Path $root "target\release\rip.exe"),
    (Join-Path $root "target\debug\rip.exe")
) | Where-Object { Test-Path $_ } | Select-Object -First 1

if (-not $exe) {
    Write-Error "未找到 rip.exe，请先在本仓库根目录执行：cargo build -p rp-cli"
}

# 当前会话内生效，不需要手动改系统 PATH。
$env:Path = (Split-Path -Parent $exe) + ";" + $env:Path

if ($args.Count -eq 0) {
    & $exe version
    exit $LASTEXITCODE
}

# 快捷方式：启动后端。工作目录必须是 server，静态资源与 SQLite 都按相对路径解析。
if ($args[0] -eq "server") {
    $rest = @()
    if ($args.Count -gt 1) {
        $rest = $args[1..($args.Count - 1)]
    }
    Push-Location (Join-Path $demo "server")
    try {
        & $exe run main.rp @rest
    } finally {
        Pop-Location
    }
    exit $LASTEXITCODE
}

& $exe @args
exit $LASTEXITCODE
