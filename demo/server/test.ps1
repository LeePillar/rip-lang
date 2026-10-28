$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$rip = (Resolve-Path (Join-Path $root "..\..\target\debug\rip.exe")).Path
$port = 18081
$base = "http://127.0.0.1:$port"

$storage = Join-Path $root "storage"
New-Item -ItemType Directory -Force -Path $storage | Out-Null

# 测试使用独立的 storage/tests/app_test.db，demo 正式库 storage/app.db
# 永远保留、不删除；tests 子目录也不会被 rip build 打进交付包。
$tests = Join-Path $storage "tests"
New-Item -ItemType Directory -Force -Path $tests | Out-Null
$testDb = Join-Path $tests "app_test.db"
if (Test-Path $testDb) {
    Remove-Item $testDb -Force
}
$env:RIP_DB = $testDb

# 上传测试写入独立的 storage/tests/uploads，不污染正式 uploads/。
$uploadDir = Join-Path $tests "uploads"
if (Test-Path $uploadDir) {
    Remove-Item $uploadDir -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $uploadDir | Out-Null
$env:RIP_UPLOAD_DIR = $uploadDir

$out = Join-Path $storage "server.log"
$err = Join-Path $storage "server.err"
$server = Start-Process -FilePath $rip `
    -ArgumentList @("run", "main.rp", "--port", "$port") `
    -WorkingDirectory $root `
    -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput $out -RedirectStandardError $err

function Invoke-Rip {
    param(
        [string]$Method,
        [string]$Path,
        [string]$Body = "",
        [string]$Token = ""
    )
    $curlArgs = @("-s", "-w", "`n%{http_code}", "-X", $Method, "$base$Path")
    if ($Token -ne "") {
        $curlArgs += @("-H", "Authorization: Bearer $Token")
    }
    if ($Body -ne "") {
        $bodyFile = Join-Path $tests "request-body.json"
        [IO.File]::WriteAllText($bodyFile, $Body, (New-Object System.Text.UTF8Encoding($false)))
        $curlArgs += @("-H", "Content-Type: application/json", "--data-binary", "@$bodyFile")
    }
    $raw = & curl.exe @curlArgs
    if ($LASTEXITCODE -ne 0) {
        throw "curl failed with exit code $LASTEXITCODE"
    }
    $lines = $raw -split "`n"
    $status = [int]$lines[$lines.Length - 1]
    $content = ($lines[0..($lines.Length - 2)] -join "`n")
    return [pscustomobject]@{ Status = $status; Body = $content }
}

function Invoke-RipUpload {
    param([string]$Path, [string]$File, [string]$Token)
    $curlArgs = @("-s", "-w", "`n%{http_code}", "-X", "POST", "$base$Path")
    if ($Token -ne "") {
        $curlArgs += @("-H", "Authorization: Bearer $Token")
    }
    $curlArgs += @("-F", "file=@$File")
    $raw = & curl.exe @curlArgs
    if ($LASTEXITCODE -ne 0) {
        throw "curl failed with exit code $LASTEXITCODE"
    }
    $lines = $raw -split "`n"
    $status = [int]$lines[$lines.Length - 1]
    $content = ($lines[0..($lines.Length - 2)] -join "`n")
    return [pscustomobject]@{ Status = $status; Body = $content }
}

function Assert-Status {
    param($Response, [int]$Expected, [string]$Label)
    if ($Response.Status -ne $Expected) {
        throw "$Label expected HTTP $Expected, got $($Response.Status): $($Response.Body)"
    }
    "PASS $Label -> $($Response.Status)"
}

function Assert-Json {
    param($Response, [string]$Path, $Expected, [string]$Label)
    $value = $Response.Body | ConvertFrom-Json
    foreach ($part in $Path.Split(".")) {
        if ($part -match '^\d+$') {
            $value = $value[[int]$part]
        } else {
            $value = $value.$part
        }
    }
    if ("$value" -ne "$Expected") {
        throw "$Label expected $Path=$Expected, got $value (body: $($Response.Body))"
    }
    "PASS $Label $Path=$value"
}

function Assert-Contains {
    param($Response, [string]$Needle, [string]$Label)
    if ($Response.Body -notlike "*$Needle*") {
        throw "$Label expected body to contain '$Needle' (body: $($Response.Body))"
    }
    "PASS $Label contains '$Needle'"
}

try {
    $ready = $false
    for ($i = 0; $i -lt 40; $i++) {
        Start-Sleep -Milliseconds 250
        try {
            $probe = Invoke-Rip -Method "GET" -Path "/"
            if ($probe.Status -eq 200) {
                $ready = $true
                break
            }
        } catch {
        }
    }
    if (-not $ready) {
        throw "server did not start: $(Get-Content $err -ErrorAction SilentlyContinue)"
    }
    "server ready on $base"

    # 前端静态资源
    $page = Invoke-Rip -Method "GET" -Path "/"
    Assert-Status $page 200 "index page"
    Assert-Contains $page "Rip 用户管理" "index page"

    $vue = Invoke-Rip -Method "GET" -Path "/vue.global.prod.js"
    Assert-Status $vue 200 "vue asset"

    $script = Invoke-Rip -Method "GET" -Path "/app.js"
    Assert-Status $script 200 "app.js asset"

    $style = Invoke-Rip -Method "GET" -Path "/style.css"
    Assert-Status $style 200 "style.css asset"

    # 登录失败
    $badLogin = Invoke-Rip -Method "POST" -Path "/api/login" `
        -Body '{"account":"admin@example.com","password":"wrong-password"}'
    Assert-Status $badLogin 401 "login with wrong password"

    # 未登录访问受保护接口
    $anonymous = Invoke-Rip -Method "GET" -Path "/api/users"
    Assert-Status $anonymous 401 "list without token"

    # 登录成功
    $login = Invoke-Rip -Method "POST" -Path "/api/login" `
        -Body '{"account":"admin@example.com","password":"admin123"}'
    Assert-Status $login 200 "login"
    $token = ($login.Body | ConvertFrom-Json).data.token
    if ([string]::IsNullOrEmpty($token)) { throw "login did not return token" }
    Assert-Json $login "data.user.role" "admin" "login"

    $me = Invoke-Rip -Method "GET" -Path "/api/me" -Token $token
    Assert-Status $me 200 "me"
    Assert-Json $me "data.email" "admin@example.com" "me"

    # 文件上传：未登录 401，登录后写入 RIP_UPLOAD_DIR
    $uploadFile = Join-Path $tests "upload-probe.txt"
    [IO.File]::WriteAllText($uploadFile, "rip upload probe", (New-Object System.Text.UTF8Encoding($false)))
    $uploadAnonymous = Invoke-RipUpload -Path "/api/upload" -File $uploadFile -Token ""
    Assert-Status $uploadAnonymous 401 "upload without token"

    $upload = Invoke-RipUpload -Path "/api/upload" -File $uploadFile -Token $token
    Assert-Status $upload 201 "upload file"
    Assert-Contains $upload "upload-probe.txt" "upload file"
    $uploadPath = Join-Path $uploadDir "upload-probe.txt"
    if (-not (Test-Path $uploadPath)) {
        throw "upload did not write $uploadPath (body: $($upload.Body))"
    }
    if ((Get-Content -Raw $uploadPath) -ne "rip upload probe") {
        throw "uploaded file content mismatch"
    }
    "PASS upload file written to $uploadPath"

    # 列表（含种子管理员）
    $list = Invoke-Rip -Method "GET" -Path "/api/users" -Token $token
    Assert-Status $list 200 "list"
    Assert-Json $list "data.total" 1 "list"

    # 新增
    $created = Invoke-Rip -Method "POST" -Path "/api/users" -Token $token `
        -Body '{"name":"Zhang San","email":"zhangsan@example.com","phone":"13900000000","password":"secret123","role":"user","status":"active"}'
    Assert-Status $created 201 "create user"
    Assert-Json $created "data.name" "Zhang San" "create user"
    $userId = ($created.Body | ConvertFrom-Json).data.id
    if ($null -eq $userId) { throw "create did not return data.id" }

    # 详情
    $detail = Invoke-Rip -Method "GET" -Path "/api/users/$userId" -Token $token
    Assert-Status $detail 200 "detail"
    Assert-Json $detail "data.email" "zhangsan@example.com" "detail"

    # 更新
    $updated = Invoke-Rip -Method "PUT" -Path "/api/users/$userId" -Token $token `
        -Body '{"name":"Zhang Sanfeng","phone":"13700000000"}'
    Assert-Status $updated 200 "update"
    Assert-Json $updated "data.name" "Zhang Sanfeng" "update"
    Assert-Json $updated "data.phone" "13700000000" "update"

    # 搜索（姓名 / 邮箱 / 手机号）
    $searchName = Invoke-Rip -Method "GET" -Path "/api/users?keyword=Sanfeng&page=1&limit=10" -Token $token
    Assert-Status $searchName 200 "search by name"
    Assert-Json $searchName "data.total" 1 "search by name"
    Assert-Json $searchName "data.items.0.name" "Zhang Sanfeng" "search by name"

    $searchEmail = Invoke-Rip -Method "GET" -Path "/api/users?keyword=zhangsan" -Token $token
    Assert-Status $searchEmail 200 "search by email"
    Assert-Json $searchEmail "data.items.0.email" "zhangsan@example.com" "search by email"

    $searchPhone = Invoke-Rip -Method "GET" -Path "/api/users?keyword=137000&status=active" -Token $token
    Assert-Status $searchPhone 200 "search by phone + status"
    Assert-Json $searchPhone "data.total" 1 "search by phone + status"

    $disabled = Invoke-Rip -Method "GET" -Path "/api/users?status=disabled" -Token $token
    Assert-Status $disabled 200 "filter disabled"
    Assert-Json $disabled "data.total" 0 "filter disabled"

    # 重复邮箱
    $duplicate = Invoke-Rip -Method "POST" -Path "/api/users" -Token $token `
        -Body '{"name":"Li Si","email":"zhangsan@example.com","password":"secret123"}'
    Assert-Status $duplicate 409 "duplicate email"

    # 校验失败
    $invalid = Invoke-Rip -Method "POST" -Path "/api/users" -Token $token `
        -Body '{"name":"A","email":"bad","password":"123"}'
    Assert-Status $invalid 400 "validation"

    # 非法 token
    $badToken = Invoke-Rip -Method "GET" -Path "/api/users" -Token "not-a-valid-token"
    Assert-Status $badToken 401 "invalid token"

    # 删除
    $deleted = Invoke-Rip -Method "DELETE" -Path "/api/users/$userId" -Token $token
    Assert-Status $deleted 200 "delete"
    Assert-Json $deleted "data.deleted" 1 "delete"

    $missing = Invoke-Rip -Method "GET" -Path "/api/users/$userId" -Token $token
    Assert-Status $missing 404 "after delete"

    # 退出登录后 token 失效
    $logout = Invoke-Rip -Method "POST" -Path "/api/logout" -Token $token
    Assert-Status $logout 200 "logout"

    $afterLogout = Invoke-Rip -Method "GET" -Path "/api/me" -Token $token
    Assert-Status $afterLogout 401 "token revoked after logout"

    "ALL TESTS PASSED"
}
finally {
    if ($server -and -not $server.HasExited) {
        Stop-Process -Id $server.Id -Force
    }
}
