<#
.SYNOPSIS
  采集渠道的**真实响应结构**：发一次（或几次）调用，把脱敏后的逐字响应写成文件。

.DESCRIPTION
  这个脚本的存在理由写在本仓库的 `out-reference/aihubmix/gpt-image-2.md` 里：
  「返回格式 aihubmix 没有提供……tests 目录写好测试文件，返回信息保存到本地文件
  （用户调试查看实际的返回格式结构），然后由用户执行。」

  它**默认只演练**：不加 `-ConfirmPaidCalls` 时只打印将要发起的调用与预算估算，
  一个请求都不发。

  这是本仓库**唯一**会发真实计费调用的入口——其余测试一律对着进程内假上游。
  凭证只从环境变量读取，绝不写入文件；结果里的 URL 与 task id 会自动脱敏。

.PARAMETER Provider
  要采集哪个渠道：aihubmix / apimart。多个用逗号分隔（如 `aihubmix,apimart`）。

.PARAMETER Probe
  要采集哪些端点，逗号分隔。缺省为所选渠道的全部：
    aihubmix → generations（同步文生图）、native_sync（`/ai/v1` 默认同步）、edits（同步图片编辑）、async（/ai/v1 异步任务）
    apimart  → image_edit（上传换 URL + 带参考图/遮罩的生成 + 轮询）

.PARAMETER ConfirmPaidCalls
  显式确认"这次会花钱"。没有它，脚本只演练。

.PARAMETER MaxPaidCalls
  本次允许的计费调用次数上限，超过即中止（默认 6）。

.PARAMETER OutDir
  落盘根目录，默认 `out-reference/`。

.EXAMPLE
  # 演练：看它打算做什么、大概花多少
  pwsh -File scripts/probe/response-shapes.ps1 -Provider aihubmix

.EXAMPLE
  # 真跑：采集 AIHubMix 的同步文生图与图片编辑
  pwsh -File scripts/probe/response-shapes.ps1 -Provider aihubmix -Probe generations,edits -ConfirmPaidCalls
#>
[CmdletBinding()]
param(
  [string] $Provider = 'aihubmix',
  [string] $Probe = '',
  [switch] $ConfirmPaidCalls,
  [int] $MaxPaidCalls = 6,
  [string] $OutDir = ''
)

$ErrorActionPreference = 'Stop'
if (-not $OutDir) { $OutDir = Join-Path (Split-Path (Split-Path $PSScriptRoot -Parent) -Parent) 'out-reference' }
$today = (Get-Date).ToString('yyyy-MM-dd')

$providerList = @($Provider -split ',' | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$unknown = $providerList | Where-Object { $_ -notin @('aihubmix', 'apimart') }
if ($unknown) { throw "不认识的 Provider：$($unknown -join ', ')（只支持 aihubmix / apimart）" }
$probeList = @($Probe -split ',' | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$knownProbes = @('generations', 'native_sync', 'edits', 'async', 'image_edit')
$unknownProbe = $probeList | Where-Object { $_ -notin $knownProbes }
if ($unknownProbe) { throw "不认识的 Probe：$($unknownProbe -join ', ')（只支持 $($knownProbes -join ' / ')）" }

# 测试图（内嵌，避免依赖本机图片文件）：256×256 纯色圆 + 256×256 带 alpha 的方形遮罩。
$TestImageBase64 = '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'
$TestMaskBase64 = 'iVBORw0KGgoAAAANSUhEUgAAAQAAAAEACAYAAABccqhmAAADtklEQVR4nO3SQRGAMAAEsfo3DSJ47A1NNOQcAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAOCbh2vV9xhQJ6RT32NAnZBOfY8BdUI69T0G1Anp1PcYUCekU99jQJ2QTn2PAXVCOvU9BtQJ6dT3GFAnpFPfY0CdkE59jwF1Qjr1PQbUCenU9xhQJ6RT32NAnZBOfY8BdUI69T0G1Anp1PcYUCekU99jQJ2QTn2PAXVCOvU9BtQJ6dT3GFAnpFPfY0CdkE59jwF1Qjr1PQbUCenU9xhQJ6RT32NAnZBOfY8BdUI69T0G1Anp1PcYUCekU99jQJ2QTn2PAXVCOvU9BtQJ6dT3GFAnpFPfY0CdkE59jwF1Qjr1PQbUCenU9xhQJ6RT32NAnZBOfY8BdUI69T0G1Anp1PcYUCekU99jQJ2QTn2PAXVCOvU9BtQJ6dT3GFAnpFPfY0CdkE59jwF1Qjr1PQbUCenU9xhQJ6RT32NAnZBOfY8BdUI69T0G1Anp1PcYUCekU99jQJ2QTn2PAXVCOvU9BtQJ6dT3GFAnpFPfY0CdkE59jwF1Qjr1PQbUCenU9xhQJ6RT32NAnZBOfY8BdUI69T0G1Anp1PcYUCekU99jQJ2QTn2PAXVCOvU9BtQJ6dT3GFAnpFPfY0CdkE59jwF1Qjr1PQbUCenU9xhQJ6RT32NAnZBOfY8BdUI69T0G1Anp1PcYUCekU99jQJ2QTn2PAXVCOvU9BtQJ6dT3GFAnpFPfY0CdkE59jwF1Qjr1PQbUCenU9xhQJ6RT32NAnZBOfY8BdUI69T0G1Anp1PcYUCekU99jQJ2QTn2PAXVCOvU9BtQJ6dT3GFAnpFPfY0CdkE59jwF1Qjr1PQbUCenU9xhQJ6RT32NAnZBOfY8BdUI69T0G1Anp1PcYUCekU99jQJ2QTn0PAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA/uAF5dgWshyAgNAAAAAASUVORK5CYII='

$probes = if ($probeList) { $probeList } else { @('generations', 'native_sync', 'edits', 'async', 'image_edit') }
$plan = @()
foreach ($p in $providerList) {
  foreach ($x in $probes) {
    if ($p -eq 'aihubmix' -and $x -in @('generations', 'native_sync', 'edits', 'async')) { $plan += [pscustomobject]@{ Provider = $p; Probe = $x } }
    if ($p -eq 'apimart' -and $x -eq 'image_edit') { $plan += [pscustomobject]@{ Provider = $p; Probe = $x } }
  }
}
if (-not $plan) { throw '没有匹配的采集项：aihubmix 支持 generations/native_sync/edits/async，apimart 支持 image_edit' }

$estimate = @{ aihubmix = 0.006; apimart = 0.011 }
$paidCalls = $plan.Count
Write-Host '将要执行的采集：'
$plan | ForEach-Object { Write-Host ("  - {0,-9} {1}" -f $_.Provider, $_.Probe) }
Write-Host ("预算估算：约 `${0:N3}（按历史实测单价：AIHubMix 约 `$0.006/次，APIMart 约 `$0.011/次）" -f (($plan | ForEach-Object { $estimate[$_.Provider] } | Measure-Object -Sum).Sum))

if (-not $ConfirmPaidCalls) {
  Write-Host ''
  Write-Host '演练模式（未加 -ConfirmPaidCalls）：一个请求都没有发。' -ForegroundColor Yellow
  Write-Host '确认要花这笔钱时，加 -ConfirmPaidCalls 重跑。'
  return
}

if ($paidCalls -gt $MaxPaidCalls) { throw "本次计划 $paidCalls 次调用，超过 -MaxPaidCalls $MaxPaidCalls" }

function Get-EnvOrThrow([string] $name) {
  $v = [Environment]::GetEnvironmentVariable($name, 'Process')
  if (-not $v) { $v = [Environment]::GetEnvironmentVariable($name, 'User') }
  if (-not $v) { throw "缺少环境变量 $name（凭证只从环境变量读取）" }
  return $v
}

# 自动脱敏：URL 只留主机，task id / b64_json 换成占位符（凭证纪律：不入库真实 URL 与 task id）。
function Protect-Json([string] $json) {
  $out = [regex]::Replace($json, 'https?://([^/"]+)[^"]*', 'https://$1/<redacted>')
  # 只替换**值**，不要把任务的字段名（如 "task_id"）也换掉。
  $out = [regex]::Replace($out, ':\s*"(task_[A-Za-z0-9]+|t_[A-Za-z0-9]+)"', ': "<task-id-redacted>"')
  $out = [regex]::Replace($out, '"(b64_json|b64|base64)"\s*:\s*"[^"]*"', '"$1": "<base64-redacted>"')
  return $out
}

function Save-Shape([string] $provider, [string] $probe, $call, $response, [int] $http, [double] $seconds, $headers) {
  $dir = Join-Path $OutDir $provider
  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  $file = Join-Path $dir ("probe-{0}-{1}.json" -f $today, $probe)
  # 响应头也留档：`x-request-id` 一类标识只可能在头里（凭证类的头一律不记）。
  $safeHeaders = [ordered]@{}
  if ($headers) {
    foreach ($name in $headers.Keys) {
      if ($name -match '(?i)auth|cookie|token|secret|key') { continue }
      $safeHeaders[$name] = ($headers[$name] -join ', ')
    }
  }
  $doc = [ordered]@{
    '_comment'           = "受控采集（$today）。URL 与 task id 已自动脱敏；凭证从未写入文件。"
    '_provider'          = $provider
    '_probe'             = $probe
    '_http'              = [ordered]@{ status = $http; elapsed_seconds = [math]::Round($seconds, 2); headers = $safeHeaders }
    '_call'              = $call
    '_terminal_response' = $response
    '_conclusions'       = @('（待填：这次采集结清了什么、与既有记录是否一致）')
  }
  $text = Protect-Json ($doc | ConvertTo-Json -Depth 12)
  [System.IO.File]::WriteAllText($file, $text)
  Write-Host ("已写入 {0}" -f $file) -ForegroundColor Green
  Write-Host ("  请接着做两件事：在 docs/facts/channel-facts.md §5 记一行调用留档；在 out-reference/{0}/response-shapes.md 登记这个文件。" -f $provider)
}

function New-TestImages([string] $dir) {
  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  $img = Join-Path $dir 'probe-image.png'
  $mask = Join-Path $dir 'probe-mask.png'
  [System.IO.File]::WriteAllBytes($img, [Convert]::FromBase64String($TestImageBase64))
  [System.IO.File]::WriteAllBytes($mask, [Convert]::FromBase64String($TestMaskBase64))
  return @{ Image = $img; Mask = $mask }
}

$work = Join-Path ([System.IO.Path]::GetTempPath()) ("seeai-probe-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
$assets = New-TestImages $work
$sw = [System.Diagnostics.Stopwatch]::new()

foreach ($item in $plan) {
  switch ("$($item.Provider)/$($item.Probe)") {
    'aihubmix/generations' {
      $key = Get-EnvOrThrow 'AIHUBMIX_API_KEY'
      $body = @{ model = 'gpt-image-2.5-flare'; prompt = 'a small blue cube on a white background'; n = 1; size = '1024x1024'; quality = 'low'; output_format = 'png' } | ConvertTo-Json
      $sw.Restart()
      $r = Invoke-WebRequest -Method Post -Uri 'https://api.inferera.com/v1/images/generations' -Headers @{ Authorization = "Bearer $key" } -ContentType 'application/json' -Body $body -TimeoutSec 600
      $sw.Stop()
      Save-Shape 'aihubmix' 'generations' ([ordered]@{ endpoint = 'POST https://api.inferera.com/v1/images/generations'; request_body = ($body | ConvertFrom-Json) }) ($r.Content | ConvertFrom-Json) $r.StatusCode $sw.Elapsed.TotalSeconds $r.Headers
    }
    'aihubmix/native_sync' {
      # `/ai/v1/images/generations` 的**默认形态**（不带 `async`）：机器 Schema 里它 `mode=sync`，
      # 只有显式 `async: true` 才返回任务对象。这条路从没实测过。
      $key = Get-EnvOrThrow 'AIHUBMIX_API_KEY'
      $body = @{ model = 'gpt-image-2.5-flare'; prompt = 'a small blue cube on a white background'; n = 1; size = '1024x1024'; output_format = 'png'; extra = @{ quality = 'low' } } | ConvertTo-Json -Depth 5
      $sw.Restart()
      $r = Invoke-WebRequest -Method Post -Uri 'https://api.inferera.com/ai/v1/images/generations' -Headers @{ Authorization = "Bearer $key" } -ContentType 'application/json' -Body $body -TimeoutSec 600
      $sw.Stop()
      Save-Shape 'aihubmix' 'native_sync' ([ordered]@{ endpoint = 'POST https://api.inferera.com/ai/v1/images/generations'; note = '不带 async：采集它的默认（同步）形态'; request_body = ($body | ConvertFrom-Json) }) ($r.Content | ConvertFrom-Json) $r.StatusCode $sw.Elapsed.TotalSeconds $r.Headers
    }
    'aihubmix/edits' {
      $key = Get-EnvOrThrow 'AIHUBMIX_API_KEY'
      $sw.Restart()
      $r = Invoke-WebRequest -Method Post -Uri 'https://api.inferera.com/v1/images/edits' -Headers @{ Authorization = "Bearer $key" } -Form @{ model = 'gpt-image-2.5-flare'; prompt = 'replace the background with a blue sky and white clouds'; image = Get-Item $assets.Image; mask = Get-Item $assets.Mask; n = 1; size = '1024x1024'; quality = 'low' } -TimeoutSec 600
      $sw.Stop()
      Save-Shape 'aihubmix' 'edits' ([ordered]@{ endpoint = 'POST https://api.inferera.com/v1/images/edits'; request_form = @{ model = 'gpt-image-2.5-flare'; prompt = '…'; image = '<probe-image.png>'; mask = '<probe-mask.png>'; n = 1; size = '1024x1024'; quality = 'low' } }) ($r.Content | ConvertFrom-Json) $r.StatusCode $sw.Elapsed.TotalSeconds
    }
    'aihubmix/async' {
      $key = Get-EnvOrThrow 'AIHUBMIX_API_KEY'
      $body = @{ model = 'gpt-image-2.5-flare'; prompt = 'a small blue cube on a white background'; n = 1; async = $true; extra = @{ quality = 'low' } } | ConvertTo-Json
      $sw.Restart()
      $created = Invoke-WebRequest -Method Post -Uri 'https://api.inferera.com/ai/v1/images/generations' -Headers @{ Authorization = "Bearer $key" } -ContentType 'application/json' -Body $body -TimeoutSec 600
      $task = $created.Content | ConvertFrom-Json
      $terminal = $task
      for ($i = 0; $i -lt 60; $i++) {
        if ($terminal.status -in @('completed', 'failed', 'cancelled')) { break }
        Start-Sleep -Seconds 5
        $terminal = (Invoke-WebRequest -Method Get -Uri ("https://api.inferera.com/ai/v1/images/{0}" -f $task.id) -Headers @{ Authorization = "Bearer $key" } -TimeoutSec 120).Content | ConvertFrom-Json
      }
      $sw.Stop()
      Save-Shape 'aihubmix' 'async' ([ordered]@{ endpoint = 'POST https://api.inferera.com/ai/v1/images/generations'; request_body = ($body | ConvertFrom-Json); create_response = $task }) $terminal $created.StatusCode $sw.Elapsed.TotalSeconds
    }
    'apimart/image_edit' {
      $key = Get-EnvOrThrow 'APIMART_API_KEY'
      $auth = @{ Authorization = "Bearer $key" }
      $sw.Restart()
      $u1 = (Invoke-WebRequest -Method Post -Uri 'https://api.apib.ai/v1/uploads/images' -Headers $auth -Form @{ file = Get-Item $assets.Image } -TimeoutSec 300).Content | ConvertFrom-Json
      $u2 = (Invoke-WebRequest -Method Post -Uri 'https://api.apib.ai/v1/uploads/images' -Headers $auth -Form @{ file = Get-Item $assets.Mask } -TimeoutSec 300).Content | ConvertFrom-Json
      $body = @{ model = 'gpt-image-2.5-flare'; prompt = 'turn the blue circle into a red apple on a wooden table'; image_urls = @($u1.url); mask_url = $u2.url; n = 1; size = '1:1'; resolution = '1k'; quality = 'low' } | ConvertTo-Json
      $submit = (Invoke-WebRequest -Method Post -Uri 'https://api.apib.ai/v1/images/generations' -Headers $auth -ContentType 'application/json' -Body $body -TimeoutSec 300).Content | ConvertFrom-Json
      $taskId = $submit.data[0].task_id
      $terminal = $submit
      for ($i = 0; $i -lt 60; $i++) {
        if ($terminal.data.status -in @('completed', 'failed', 'cancelled')) { break }
        Start-Sleep -Seconds 3
        $terminal = (Invoke-WebRequest -Method Get -Uri ("https://api.apib.ai/v1/tasks/{0}" -f $taskId) -Headers $auth -TimeoutSec 120).Content | ConvertFrom-Json
      }
      $sw.Stop()
      Save-Shape 'apimart' 'image_edit' ([ordered]@{ endpoint = 'POST https://api.apib.ai/v1/images/generations'; uploads = @([ordered]@{ filename = $u1.filename; content_type = $u1.content_type; bytes = $u1.bytes }, [ordered]@{ filename = $u2.filename; content_type = $u2.content_type; bytes = $u2.bytes }); request_body = ($body | ConvertFrom-Json); create_response = $submit }) $terminal 200 $sw.Elapsed.TotalSeconds
    }
  }
}

Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
Write-Host ''
Write-Host '采集完成。别忘了：docs/facts/channel-facts.md §5 记调用留档；response-shapes.md 登记文件。' -ForegroundColor Yellow
