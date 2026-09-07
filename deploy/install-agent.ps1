<#
.SYNOPSIS
  Install the PatchPanel agent as a Windows Service.

.DESCRIPTION
  Run from an elevated PowerShell prompt. Re-running upgrades the binary in
  place and restarts the service, keeping the agent's existing identity in
  C:\ProgramData\PatchPanel.

.EXAMPLE
  .\install-agent.ps1 -Portal ws://portal.example.com:8080/api/agent/ws -Token abc123 -Site hq
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory)][string] $Portal,
  [Parameter(Mandatory)][string] $Token,
  [string] $Site = "",
  [string] $InstallDir = "$env:ProgramFiles\PatchPanel"
)

$ErrorActionPreference = 'Stop'

$identity  = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  throw "This script must run from an elevated prompt (it installs a service)."
}

$source = Join-Path $PSScriptRoot '..\target\release\pp-agent.exe'
if (-not (Test-Path $source)) {
  throw "Missing $source - run 'cargo build --release -p pp-agent' first."
}

$service = 'PatchPanelAgent'
$target  = Join-Path $InstallDir 'pp-agent.exe'

# Stop first: Windows will not let us overwrite a running image.
if (Get-Service -Name $service -ErrorAction SilentlyContinue) {
  Write-Host "Stopping existing service..."
  Stop-Service -Name $service -Force -ErrorAction SilentlyContinue
  # The SCM reports Stopped slightly before the image is released.
  Start-Sleep -Seconds 2
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Copy-Item -Path $source -Destination $target -Force

Write-Host "Enrolling against $Portal ..."
& $target enroll --portal $Portal --token $Token --site $Site
if ($LASTEXITCODE -ne 0) { throw "Enrollment failed with exit code $LASTEXITCODE." }

# The config holds the enrollment secret; keep it to Administrators and SYSTEM.
$config = Join-Path $env:ProgramData 'PatchPanel\agent.json'
if (Test-Path $config) {
  icacls $config /inheritance:r /grant:r "SYSTEM:(F)" "Administrators:(F)" | Out-Null
}

if (-not (Get-Service -Name $service -ErrorAction SilentlyContinue)) {
  Write-Host "Registering service..."
  & $target install-service
  if ($LASTEXITCODE -ne 0) { throw "Service registration failed with exit code $LASTEXITCODE." }
}

Start-Service -Name $service
Write-Host ""
Write-Host "Installed. Check it with:"
Write-Host "  Get-Service $service"
Write-Host "  Get-EventLog -LogName Application -Source $service -Newest 20"
Write-Host ""
Write-Host "For OS patching (not just app updates), install the Windows Update module:"
Write-Host "  Install-Module PSWindowsUpdate -Force -Scope AllUsers"
