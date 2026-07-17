#!/usr/bin/env pwsh
# pp-box.ps1 — run and manage a PurePrivacy box in Docker on Windows (native PowerShell).
# Mirrors ./pp-box (bash). Needs Docker Desktop. Full guide: docker/README.md.
#
#   .\pp-box.ps1 init            create .env (admin user/pass + a fresh secrets key)
#   .\pp-box.ps1 build           build the image (needs Linux-staged artifacts — see below)
#   .\pp-box.ps1 up              start the box (mints its onion on first run)
#   .\pp-box.ps1 qr              print the phone-connect QR
#   .\pp-box.ps1 status          running? onion, uptime
#   .\pp-box.ps1 logs            follow the logs
#   .\pp-box.ps1 restart | down  restart / stop (identity kept in the volume)
#   .\pp-box.ps1 update          rebuild + recreate on the same volume
#   .\pp-box.ps1 backup [dir]    tar the volume (onion key + secrets + pairings) — DO THIS
#   .\pp-box.ps1 restore <file>  restore a backup (box must be down)
#   .\pp-box.ps1 shell | destroy shell in the container / remove box + volume (asks first)

[CmdletBinding()]
param(
  [Parameter(Position = 0)][string]$Command = 'help',
  [Parameter(Position = 1, ValueFromRemainingArguments = $true)][string[]]$Rest
)
$ErrorActionPreference = 'Stop'
# PS 7.4+ turns a non-zero native exit into a THROW under Stop — which would break our own
# `$LASTEXITCODE` checks (docker image/volume inspect exit 1 to mean "absent"). Opt out.
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -Scope Global -ErrorAction SilentlyContinue) {
  $PSNativeCommandUseErrorActionPreference = $false
}
$Here   = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $Here                      # so `docker compose` uses this dir's file + .env
$Volume = 'pureprivacy-data'
$Image  = if ($env:IMAGE) { $env:IMAGE } else { 'pureprivacy-box:dev' }

function Die($m)        { Write-Host "error: $m" -ForegroundColor Red; exit 1 }
function Have-Env       { if (-not (Test-Path "$Here\.env")) { Die "no .env yet — run: .\pp-box.ps1 init" } }
function Image-Exists   { docker image inspect $Image *> $null; return ($LASTEXITCODE -eq 0) }
function Box-Running    { return ((docker ps --format '{{.Names}}' 2>$null) -contains 'pureprivacy-box') }
function Volume-Exists  { docker volume inspect $Volume *> $null; return ($LASTEXITCODE -eq 0) }
function Env-Val($k) {
  if (-not (Test-Path "$Here\.env")) { return '' }
  $m = Select-String -Path "$Here\.env" -Pattern "^$k=" | Select-Object -First 1
  if ($m) { return ($m.Line -replace "^$k=", '') } else { return '' }
}
function Onion {
  $j = docker exec pureprivacy-box sh -c 'cat /data/box.json 2>/dev/null' 2>$null
  if ($j -and ($j -match '"onion"[^"]*"([a-z2-7]+\.onion)"')) { return $Matches[1] } else { return '' }
}

switch ($Command) {

  'init' {
    if ((Test-Path "$Here\.env") -and ($Rest -notcontains '--force')) {
      Die ".env already exists (use: .\pp-box.ps1 init --force to overwrite — this rotates the secrets key!)"
    }
    $u = Read-Host "Phone login username [jaime]"; if (-not $u) { $u = 'jaime' }
    $b = Read-Host "Box name [mybox]";             if (-not $b) { $b = 'mybox' }
    $sec = Read-Host "Box admin password (what the phone signs in with)" -AsSecureString
    $p = [System.Net.NetworkCredential]::new('', $sec).Password
    if (-not $p) { Die "a password is required" }
    $bytes = New-Object byte[] 32
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
    $key = [Convert]::ToBase64String($bytes)
    $body = @"
# PurePrivacy box config — read automatically by docker compose. KEEP THIS PRIVATE.
# PP_SECRETS_KEY decrypts secrets.json; it must stay the SAME across restarts. If you lose
# it you cannot decrypt the box's secrets — back it up together with the volume.
PP_USER=$u
PP_BOX=$b
PP_PASS=$p
PP_SECRETS_KEY=$key
"@
    # UTF-8 without BOM — a BOM would corrupt the first line for docker compose.
    [System.IO.File]::WriteAllText("$Here\.env", $body, (New-Object System.Text.UTF8Encoding $false))
    if (Image-Exists) { Write-Host "OK wrote .env. Next: .\pp-box.ps1 up" }
    else              { Write-Host "OK wrote .env. Next: .\pp-box.ps1 build   (then: .\pp-box.ps1 up)" }
  }

  'build' {
    # The image bundles a LINUX box binary + sidecars that build.sh stages from a Linux host;
    # they aren't produced on Windows. If someone copied a Linux-built docker/bin + docker/
    # pureprivacy here we can still `docker build`; otherwise explain the Linux/WSL2 route.
    if ((Test-Path "$Here\pureprivacy") -and (Test-Path "$Here\bin")) {
      docker build -t $Image $Here
      if ($LASTEXITCODE -ne 0) { Die "docker build failed" }
      Write-Host "OK built $Image"
    } else {
      Die @"
Can't build the image on native Windows: it bundles a Linux box binary + sidecars that are
staged on a Linux host (they don't exist on Windows). Get the image one of these ways:
  * Build it under WSL2 (real Linux):  cd docker && ./pp-box build
  * Or build on any Linux host, then move it over:
        docker save $Image -o pp-box.tar   # on Linux
        docker load -i pp-box.tar          # here on Windows
  * Or push it to a registry on Linux and 'docker pull' here.
Then: .\pp-box.ps1 init ; .\pp-box.ps1 up
"@
    }
  }

  { $_ -in 'up', 'start' } {
    Have-Env
    docker compose up -d
    Write-Host "OK box starting. Watch it mint its onion + print the QR:  .\pp-box.ps1 logs"
    Write-Host "  once it's up:  .\pp-box.ps1 qr"
  }

  'qr' {
    Have-Env
    $o = Onion; $u = Env-Val 'PP_USER'
    if (-not $o) { Die "no onion yet — the box is still minting it (give it a minute), or it isn't running (.\pp-box.ps1 up)" }
    Write-Host "Scan this in the PurePrivacy phone app to connect:"
    Write-Host "  @${u}:${o}"
    docker exec pureprivacy-box qrencode -t ANSIUTF8 "pureprivacy:@${u}:${o}"
  }

  'status' {
    if (-not (Box-Running)) { Write-Host "* box: not running   (start it: .\pp-box.ps1 up)"; break }
    $up = docker ps --filter name=pureprivacy-box --format '{{.Status}}'
    $o = Onion; $u = Env-Val 'PP_USER'
    Write-Host "* box: running   ($up)"
    if ($o) { Write-Host "  user:  @${u}:${o}" } else { Write-Host "  user:  @${u}:<minting onion...>" }
    Write-Host "  volume: $Volume   (identity lives here — back it up: .\pp-box.ps1 backup)"
  }

  'logs'    { docker compose logs -f --tail=200 }
  'restart' { docker compose restart; Write-Host "OK restarted" }
  { $_ -in 'down', 'stop' } { docker compose down; Write-Host "OK stopped (identity kept in the $Volume volume)" }

  'update' {
    Have-Env
    & "$Here\pp-box.ps1" build
    docker compose up -d --force-recreate
    Write-Host "OK rebuilt + recreated on the same volume (same onion, same account)"
  }

  'backup' {
    if (-not (Volume-Exists)) { Die "no $Volume volume yet (has the box ever run?)" }
    $dir = if ($Rest -and $Rest[0]) { $Rest[0] } else { "$Here\backups" }
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $dir = (Resolve-Path $dir).Path            # absolute — Docker Desktop needs it for -v
    $o = Onion; $tag = if ($o) { ($o -replace '\.onion$','').Substring(0, [Math]::Min(12, ($o -replace '\.onion$','').Length)) } else { 'box' }
    $n = 1; while (Test-Path "$dir\pp-box-$tag-$n.tgz") { $n++ }
    $name = "pp-box-$tag-$n.tgz"
    docker run --rm -v "${Volume}:/data:ro" -v "${dir}:/backup" alpine tar czf "/backup/$name" -C /data .
    if ($LASTEXITCODE -ne 0) { Die "backup failed" }
    Write-Host "OK backed up the box identity -> $dir\$name"
    Write-Host "  (this holds the onion KEY + secrets + pairings — store it somewhere safe + private)"
  }

  'restore' {
    $file = if ($Rest) { $Rest[0] } else { '' }
    if (-not $file -or -not (Test-Path $file)) { Die "usage: .\pp-box.ps1 restore <backup.tgz>" }
    if (Box-Running) { Die "stop the box first so the restore is clean:  .\pp-box.ps1 down" }
    docker volume create $Volume | Out-Null
    $full = (Resolve-Path $file).Path
    $dir = Split-Path -Parent $full; $base = Split-Path -Leaf $full
    $ans = Read-Host "Restore '$base' INTO $Volume (overwrites its current contents)? [y/N]"
    if ($ans -ne 'y') { Write-Host "aborted"; break }
    docker run --rm -v "${Volume}:/data" -v "${dir}:/backup" alpine sh -c "rm -rf /data/* /data/..?* 2>/dev/null; tar xzf '/backup/$base' -C /data"
    if ($LASTEXITCODE -ne 0) { Die "restore failed" }
    Write-Host "OK restored into $Volume. Start it:  .\pp-box.ps1 up"
  }

  'shell' {
    docker exec -it pureprivacy-box bash
    if ($LASTEXITCODE -ne 0) { docker exec -it pureprivacy-box sh }
  }

  'destroy' {
    Write-Host "This removes the box AND its $Volume volume — the onion key + account are GONE."
    $ans = Read-Host "Type the box name to confirm ('$(Env-Val 'PP_BOX')')"
    if ($ans -and ($ans -eq (Env-Val 'PP_BOX'))) {
      docker compose down -v 2>$null
      docker volume rm $Volume 2>$null
      Write-Host "OK box destroyed. (Your .env is kept — delete it by hand if you want it gone too.)"
    } else { Write-Host "aborted" }
  }

  default {
    # print the header comment block (lines starting with #, after the shebang)
    Get-Content $MyInvocation.MyCommand.Path | Select-Object -Skip 1 |
      Where-Object { $_ -match '^#' } | ForEach-Object { $_ -replace '^# ?', '' }
  }
}
