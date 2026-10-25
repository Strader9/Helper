[CmdletBinding()]
param(
    [string]$Python = "python"
)

$ErrorActionPreference = "Stop"
$jarvisRoot = Split-Path -Parent $PSScriptRoot
$pythonRoot = Join-Path $jarvisRoot "python"
$venvPython = Join-Path $pythonRoot ".venv\Scripts\python.exe"
$configTemplate = Join-Path $pythonRoot "config\default.toml"
$localConfig = Join-Path $pythonRoot "config\local.toml"

& $Python -m venv (Join-Path $pythonRoot ".venv")
& $venvPython -m pip install --upgrade pip
& $venvPython -m pip install -e $jarvisRoot

if (-not (Test-Path -LiteralPath $localConfig)) {
    Copy-Item -LiteralPath $configTemplate -Destination $localConfig
    Write-Host "已创建 $localConfig"
    Write-Host "请编辑 local.toml：设置 native.mode=process，并填写本机 worker_path 与 model_path。"
}

Write-Host "Jarvis Python 环境已就绪：$venvPython"
