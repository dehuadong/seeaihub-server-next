#!/usr/bin/env pwsh
#
# PostgreSQL 备份与保留：导出一份自定义格式转储，并按保留天数清掉更旧的转储。
#
# 用法：
#   pwsh scripts/backup/pg-backup.ps1 [-TargetDir <目录>] [-RetentionDays <天数>] [-Keep]
#
# 连接串只从环境变量 `DATABASE_URL` 读（与运行时同一处），不从参数或配置读、不打印到输出：
# 转储里含业务数据，凭据不进命令行历史。
#
# 导出方式用 PATH 上的 `pg_dump`；找不到或导出失败时明确报错，不静默产出空文件。

[CmdletBinding()]
param(
    # 转储存放目录；默认放在仓库的本地数据目录（不入版本控制）。
    [string] $TargetDir = (Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) '.data/backups'),
    # 保留天数：比它更旧的转储会被删掉。默认 7 天；这是运维取值，按需要覆盖。
    [int] $RetentionDays = 7,
    # 只导出，不清理。
    [switch] $Keep
)

$ErrorActionPreference = 'Stop'

if (-not $env:DATABASE_URL) {
    throw 'DATABASE_URL 未设置：备份需要与运行时同一个连接串来源。'
}

if (-not (Test-Path $TargetDir)) {
    New-Item -ItemType Directory -Path $TargetDir -Force | Out-Null
}

$stamp = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')
$dump = Join-Path $TargetDir "seeai-$stamp.dump"

# 导出前清掉上一次的半成品文件，失败时也不留空转储——空文件看起来像备份成功，最危险。
if (-not (Get-Command pg_dump -ErrorAction SilentlyContinue)) {
    throw '找不到 pg_dump：备份需要 PATH 上有与服务端同大版本的 pg_dump。'
}
if (Test-Path $dump) { Remove-Item -LiteralPath $dump -Force }
& pg_dump --format=custom --file $dump $env:DATABASE_URL
if ($LASTEXITCODE -ne 0 -or -not (Test-Path $dump) -or (Get-Item $dump).Length -le 0) {
    if (Test-Path $dump) { Remove-Item -LiteralPath $dump -Force }
    throw "导出没成功（exit $LASTEXITCODE）：检查 DATABASE_URL 与 pg_dump 版本。"
}

$info = Get-Item $dump
if ($info.Length -le 0) { throw "转储是空文件：$dump" }
Write-Host ("备份完成：{0}（{1:N0} 字节）" -f $info.FullName, $info.Length)

if ($Keep) {
    Write-Host '按 -Keep：本次不清理旧转储。'
    return
}

$cutoff = (Get-Date).ToUniversalTime().AddDays(-$RetentionDays)
$old = Get-ChildItem -Path $TargetDir -Filter 'seeai-*.dump' |
    Where-Object { $_.LastWriteTimeUtc -lt $cutoff }
foreach ($file in $old) {
    Remove-Item -LiteralPath $file.FullName -Force
    Write-Host ("清理旧转储：{0}" -f $file.Name)
}
Write-Host ("保留 {0} 天内的转储；当前目录里有 {1} 份。" -f $RetentionDays, (Get-ChildItem -Path $TargetDir -Filter 'seeai-*.dump').Count)
