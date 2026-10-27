# Script de Build, Sincronizacao e Publicacao do DuckDNS Updater
param (
    [string]$Notes = "Atualizacao automatica e melhorias de estabilidade",
    [switch]$Push = $false
)

$ErrorActionPreference = "Stop"

# 1. Carrega versao do Cargo.toml
$cargoToml = Get-Content "Cargo.toml" -Raw
if ($cargoToml -match 'version\s*=\s*"([^"]+)"') {
    $version = $matches[1]
    Write-Host "[*] Versao detectada no Cargo.toml: v$version" -ForegroundColor Cyan
} else {
    Write-Error "Nao foi possivel detectar a versao no Cargo.toml"
    exit 1
}

# 2. Executa build e verificacao completa do PE / Icone
Write-Host "[*] Executando pipeline de compilacao e verificacao..." -ForegroundColor Yellow
& .\build_and_verify.ps1 -Notes $Notes
if ($LASTEXITCODE -ne 0) {
    Write-Error "Falha na compilacao/verificacao do binario"
    exit 1
}

$outputBinary = "dist\duckdns-updater.exe"
if (-not (Test-Path $outputBinary)) {
    Write-Error "Binario de distribuicao nao encontrado em $outputBinary"
    exit 1
}

# 3. Commit Local
Write-Host "[*] Registrando alteracoes no Git local..." -ForegroundColor Cyan
git add Cargo.toml Cargo.lock duckdns-updater.exe dist/duckdns-updater.exe src/ assets/ build.rs README.md GEMINI.md AGENTS.md build_and_verify.ps1 build_and_release.ps1 .gitignore eframe-patched/ .cargo/
git commit -m "release: v$version - $Notes"

if ($Push) {
    # 4. Publicacao no Git (leandrop2103/duckdns-updater)
    Write-Host "[*] Enviando alteracoes para git (leandrop2103/duckdns-updater)..." -ForegroundColor Cyan
    git push origin master
    git tag -a "v$version" -m "Release v$version"
    git push origin "v$version"

    # 5. Publicacao da Release no GitHub (leandrop2103/duckdns-updater)
    Write-Host "[*] Publicando release v$version no GitHub..." -ForegroundColor Cyan
    gh release create "v$version" "$outputBinary" --repo "leandrop2103/duckdns-updater" --title "DuckDNS Updater v$version" --notes "$Notes"

    Write-Host "`n[SUCESSO] DuckDNS Updater v$version publicado com sucesso!" -ForegroundColor Green
} else {
    Write-Host "`n[*] Push automatico desativado. Alteracoes commitadas localmente. Para enviar ao GitHub, execute: .\build_and_release.ps1 -Push" -ForegroundColor Yellow
}
