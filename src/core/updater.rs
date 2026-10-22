//! # Módulo de Atualização Automática (`core/updater.rs`)
//!
//! Este módulo gerencia a verificação, download e substituição do executável da aplicação
//! diretamente dos Releases oficiais do GitHub com fallbacks e retry transparente.
//!
//! ## Técnica de Substituição Quente no Windows:
//! No Windows, um executável em execução não pode ser sobrescrito diretamente.
//! 1. O download é gravado diretamente em arquivo temporário `duckdns-updater.exe.new`.
//! 2. Renomeamos o executável atual para `duckdns-updater.exe.old`.
//! 3. Movemos `duckdns-updater.exe.new` para `duckdns-updater.exe`.
//! 4. Disparamos a nova versão atualizada com a flag `--cleanup-old`.
//! 5. O novo processo aguarda a liberação e remove `duckdns-updater.exe.old` do disco.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::process::Command;
use std::time::Duration;

/// Repositórios de distribuição no GitHub (ordem de prioridade com espelhos de contingência)
pub const GITHUB_DIST_REPOS: &[&str] = &[
    "leandrop2103/duckdns-updater",
    "lpl2103/duckdns-updater",
];

/// Informações sobre uma versão remota disponível
#[derive(Debug, Clone)]
pub struct RemoteVersionInfo {
    pub version: String,
    pub download_url: String,
    pub release_notes: String,
}

/// Status do processo de atualização
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateStatus {
    /// Ocioso / Pronto para checar
    Idle,
    /// Baixando o novo executável (Progresso de 0.0 a 1.0)
    Downloading(f32),
    /// Atualização concluída com sucesso (Aguardando reinicialização)
    Success(String),
    /// Falha no processo de atualização (Mensagem de erro)
    Error(String),
}

/// Compara duas versões em formato SemVer ("1.1.0" vs "1.0.0").
pub fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let clean = v.trim().trim_start_matches(['v', 'V']);
    let mut parts = clean.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

pub fn is_newer_version(remote: &str, current: &str) -> bool {
    if let (Some(r), Some(c)) = (parse_version(remote), parse_version(current)) {
        r > c
    } else {
        remote.trim() != current.trim()
    }
}

/// URL da API "latest release" para um determinado repositório
fn github_api_latest(repo: &str) -> String {
    format!("https://api.github.com/repos/{}/releases/latest", repo)
}

/// URL de download direto do binário na última release
fn github_direct_download(repo: &str) -> String {
    format!(
        "https://github.com/{}/releases/latest/download/duckdns-updater.exe",
        repo
    )
}

/// Verifica se existe uma nova versão disponível nos repositórios GitHub em ordem de prioridade.
pub fn check_for_updates() -> Option<RemoteVersionInfo> {
    let current_version = env!("CARGO_PKG_VERSION");

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(4))
        .timeout(Duration::from_secs(8))
        .build();

    for repo in GITHUB_DIST_REPOS {
        let api_url = github_api_latest(repo);
        let resp = match agent
            .get(&api_url)
            .set("User-Agent", &format!("DuckDnsUpdater/{}", current_version))
            .set("Accept", "application/vnd.github.v3+json")
            .call()
        {
            Ok(r) if r.status() == 200 => r,
            _ => continue,
        };

        let Ok(json_str) = resp.into_string() else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&json_str) else {
            continue;
        };

        let tag_name = json.get("tag_name").and_then(|v| v.as_str()).unwrap_or("");
        let clean_tag = tag_name.trim().trim_start_matches(['v', 'V']);
        let body = json
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("Melhorias e correções de estabilidade.");

        let mut dl_url = github_direct_download(repo);
        if let Some(assets) = json.get("assets").and_then(|a| a.as_array()) {
            for asset in assets {
                if asset.get("name").and_then(|n| n.as_str()) == Some("duckdns-updater.exe") {
                    if let Some(url) = asset.get("browser_download_url").and_then(|u| u.as_str()) {
                        dl_url = url.to_string();
                        break;
                    }
                }
            }
        }

        if is_newer_version(clean_tag, current_version) {
            return Some(RemoteVersionInfo {
                version: clean_tag.to_string(),
                download_url: dl_url,
                release_notes: body.to_string(),
            });
        }
        return None;
    }

    None
}

/// Executa o download da nova versão com fallback de espelhos e realiza a substituição a quente do executável.
pub fn perform_auto_update<F>(custom_url: Option<String>, progress_callback: F) -> Result<(), String>
where
    F: Fn(UpdateStatus) + Send + 'static,
{
    progress_callback(UpdateStatus::Downloading(0.0));

    let mut candidate_urls: Vec<String> = Vec::new();
    if let Some(url) = custom_url {
        candidate_urls.push(url);
    }
    for repo in GITHUB_DIST_REPOS {
        let url = github_direct_download(repo);
        if !candidate_urls.contains(&url) {
            candidate_urls.push(url);
        }
    }

    let agent = ureq::AgentBuilder::new()
        .redirects(5)
        .timeout(Duration::from_secs(90))
        .build();

    let mut response_opt = None;
    let mut last_err = String::new();

    for download_url in &candidate_urls {
        match agent
            .get(download_url)
            .set("User-Agent", "DuckDnsUpdater/Updater")
            .call()
        {
            Ok(resp) if resp.status() == 200 => {
                response_opt = Some(resp);
                break;
            }
            Ok(resp) => {
                last_err = format!("HTTP {} ao acessar {}", resp.status(), download_url);
            }
            Err(e) => {
                last_err = format!("Erro ao acessar {}: {}", download_url, e);
            }
        }
    }

    let response = response_opt.ok_or_else(|| {
        format!(
            "Falha ao conectar aos servidores de atualização: {}",
            last_err
        )
    })?;

    let content_length = response
        .header("Content-Length")
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(0);

    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Falha ao determinar caminho do executável atual: {}", e))?;
    let old_exe = current_exe.with_extension("exe.old");
    let new_exe = current_exe.with_extension("exe.new");

    // Gravação em streaming direto para arquivo temporário no disco
    {
        let file = File::create(&new_exe)
            .map_err(|e| format!("Falha ao criar arquivo temporário {:?}: {}", new_exe, e))?;
        let mut writer = BufWriter::new(file);
        let mut reader = response.into_reader();
        let mut buffer = [0u8; 16384];
        let mut total_read = 0;
        let mut header_check = [0u8; 2];
        let mut checked_header = false;

        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    if !checked_header && n >= 2 {
                        header_check[0] = buffer[0];
                        header_check[1] = buffer[1];
                        checked_header = true;
                    }

                    writer
                        .write_all(&buffer[..n])
                        .map_err(|e| format!("Erro ao gravar dados do download: {}", e))?;
                    total_read += n;

                    if content_length > 0 {
                        let progress = (total_read as f32) / (content_length as f32);
                        progress_callback(UpdateStatus::Downloading(progress.min(1.0)));
                    }
                }
                Err(e) => return Err(format!("Erro durante o download do fluxo de dados: {}", e)),
            }
        }

        writer
            .flush()
            .map_err(|e| format!("Falha ao descarregar buffers do executável baixado: {}", e))?;

        // Validação de sanidade do executável PE Windows (começa com "MZ" - 0x4D, 0x5A)
        if total_read < 1024 || header_check != [0x4D, 0x5A] {
            let _ = fs::remove_file(&new_exe);
            return Err("Arquivo baixado não é um executável Windows válido ou está corrompido.".to_string());
        }
    }

    // Substituição do executável no Windows
    if old_exe.exists() {
        let _ = fs::remove_file(&old_exe);
    }

    // Renomeia atual -> old
    fs::rename(&current_exe, &old_exe)
        .map_err(|e| format!("Falha ao mover executável atual para backup: {}", e))?;

    // Renomeia new -> atual
    if let Err(e) = fs::rename(&new_exe, &current_exe) {
        let _ = fs::rename(&old_exe, &current_exe);
        return Err(format!("Falha ao posicionar novo executável: {}", e));
    }

    // Inicia o novo processo
    let _ = Command::new(&current_exe)
        .arg("--cleanup-old")
        .spawn()
        .map_err(|e| format!("Falha ao iniciar processo atualizado: {}", e))?;

    progress_callback(UpdateStatus::Success(
        "Atualização concluída com sucesso! Reiniciando...".to_string(),
    ));

    std::thread::sleep(Duration::from_millis(300));
    std::process::exit(0);
}

/// Remove arquivos temporários de atualização anterior (.exe.old)
pub fn clean_old_update_files() {
    if let Ok(exe_path) = std::env::current_exe() {
        let old_exe = exe_path.with_extension("exe.old");
        let new_exe = exe_path.with_extension("exe.new");

        if new_exe.exists() {
            let _ = fs::remove_file(&new_exe);
        }

        if old_exe.exists() {
            let is_cleanup_arg = std::env::args().any(|a| a == "--cleanup-old");
            std::thread::spawn(move || {
                if is_cleanup_arg {
                    std::thread::sleep(Duration::from_millis(1000));
                }
                for _ in 0..15 {
                    if fs::remove_file(&old_exe).is_ok() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            });
        }
    }
}
