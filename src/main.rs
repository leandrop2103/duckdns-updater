#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod core;
mod gui;

use eframe::egui;
use gui::app::DuckDnsApp;

const MESA_FILES: &[(&str, &str)] = &[
    ("opengl32.dll", "Mesa3D OpenGL stub"),
    (
        "libgallium_wgl.dll",
        "Mesa3D Gallium (llvmpipe software renderer)",
    ),
    ("dxil.dll", "DirectX IL compiler"),
];

fn main() -> eframe::Result<()> {
    core::updater::clean_old_update_files();

    let args: Vec<String> = std::env::args().collect();

    #[cfg(target_os = "windows")]
    attach_console_if_needed();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(());
    }

    if args.iter().any(|a| a == "--daemon" || a == "-d") {
        run_daemon();
        return Ok(());
    }

    let using_mesa = setup_mesa_if_needed();

    let config = core::config::AppConfig::load();
    let is_minimized_arg = args.iter().any(|a| a == "--minimized");
    let start_minimized = config.start_minimized || is_minimized_arg;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DuckDNS Updater")
            .with_inner_size([490.0, 560.0])
            .with_min_inner_size([460.0, 500.0])
            .with_resizable(false)
            .with_visible(!start_minimized)
            .with_icon(load_app_icon()),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    let run_res = eframe::run_native(
        "DuckDNS Updater",
        options,
        Box::new(|cc| {
            clear_crash_flag();
            Ok(Box::new(DuckDnsApp::new(cc)))
        }),
    );

    match run_res {
        Ok(()) => Ok(()),
        Err(e) => {
            let err_str = e.to_string();
            if !using_mesa
                && (err_str.contains("opengl 2.0")
                    || err_str.contains("OpenGL")
                    || err_str.contains("glow"))
            {
                let _ = std::fs::write(get_crash_flag_path(), "opengl_init");

                let should_download = ask_yes_no(
                    "DuckDNS Updater - Modo de Compatibilidade",
                    "O driver de vídeo deste computador não possui suporte a OpenGL 2.0+ (comum em máquinas virtuais ou drivers legados).\n\n\
                    Deseja baixar os componentes de compatibilidade gráfica (Mesa3D) agora?",
                );

                if should_download {
                    let mesa_dir = get_mesa_dir();
                    match core::mesa_downloader::download_mesa_with_progress(mesa_dir.clone()) {
                        Ok(()) => {
                            clear_crash_flag();
                            ensure_mesa_in_exe_dir_or_relaunch();
                            return Ok(());
                        }
                        Err(err) => {
                            let msg = format!(
                                "Falha no download dos componentes de compatibilidade:\n{}\n\n\
                                SOLUÇÃO MANUAL:\n\
                                1. Utilize a pasta completa 'dist' (que contém opengl32.dll, libgallium_wgl.dll e dxil.dll);\n\
                                OU\n\
                                2. Copie esses 3 arquivos para a mesma pasta do duckdns-updater.exe.",
                                err
                            );
                            show_messagebox("DuckDNS Updater - Erro de Download", &msg, true);
                            return Ok(());
                        }
                    }
                }
            }
            Err(e)
        }
    }
}

// =====================================================================================
// MessageBox e diálogos nativos do Windows
// =====================================================================================

#[cfg(target_os = "windows")]
fn show_messagebox(title: &str, message: &str, is_error: bool) {
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;

    let title_wide: Vec<u16> = OsStr::new(title).encode_wide().chain(once(0)).collect();
    let message_wide: Vec<u16> = OsStr::new(message).encode_wide().chain(once(0)).collect();

    unsafe {
        #[link(name = "user32")]
        extern "system" {
            fn MessageBoxW(
                hwnd: *mut std::ffi::c_void,
                text: *const u16,
                caption: *const u16,
                utype: u32,
            ) -> i32;
        }
        let flags: u32 = if is_error { 0x10 } else { 0x40 };
        MessageBoxW(
            std::ptr::null_mut(),
            message_wide.as_ptr(),
            title_wide.as_ptr(),
            flags,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn show_messagebox(_title: &str, message: &str, _is_error: bool) {
    eprintln!("{}", message);
}

#[cfg(target_os = "windows")]
fn ask_yes_no(title: &str, message: &str) -> bool {
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;

    let title_wide: Vec<u16> = OsStr::new(title).encode_wide().chain(once(0)).collect();
    let message_wide: Vec<u16> = OsStr::new(message).encode_wide().chain(once(0)).collect();

    unsafe {
        #[link(name = "user32")]
        extern "system" {
            fn MessageBoxW(
                hwnd: *mut std::ffi::c_void,
                text: *const u16,
                caption: *const u16,
                utype: u32,
            ) -> i32;
        }
        // MB_YESNO (0x00000004) | MB_ICONQUESTION (0x00000020)
        let ret = MessageBoxW(
            std::ptr::null_mut(),
            message_wide.as_ptr(),
            title_wide.as_ptr(),
            0x00000024,
        );
        ret == 6 // IDYES = 6
    }
}

#[cfg(not(target_os = "windows"))]
fn ask_yes_no(_title: &str, _message: &str) -> bool {
    false
}

// =====================================================================================
// Gerenciamento e Fallback do Mesa3D (Software Rendering para GPUs Legadas)
// =====================================================================================

fn get_mesa_dir() -> std::path::PathBuf {
    core::config::AppConfig::get_config_dir().join("mesa")
}

fn get_exe_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
}

fn are_mesa_files_in(dir: &std::path::Path) -> bool {
    MESA_FILES.iter().all(|(name, _)| dir.join(name).exists())
}

#[cfg(target_os = "windows")]
fn hide_file(path: &std::path::Path) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = OsStr::new(path.as_os_str())
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetFileAttributesW(lpFileName: *const u16) -> u32;
            fn SetFileAttributesW(lpFileName: *const u16, dwFileAttributes: u32) -> i32;
        }
        const INVALID_FILE_ATTRIBUTES: u32 = 0xFFFFFFFF;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x00000002;

        let attrs = GetFileAttributesW(wide.as_ptr());
        let new_attrs = if attrs != INVALID_FILE_ATTRIBUTES {
            attrs | FILE_ATTRIBUTE_HIDDEN
        } else {
            FILE_ATTRIBUTE_HIDDEN
        };
        SetFileAttributesW(wide.as_ptr(), new_attrs);
    }
}

#[cfg(not(target_os = "windows"))]
fn hide_file(_path: &std::path::Path) {}

fn sync_mesa_to_appdata() {
    if let Some(exe_dir) = get_exe_dir() {
        if are_mesa_files_in(&exe_dir) {
            let app_mesa_dir = get_mesa_dir();
            let _ = std::fs::create_dir_all(&app_mesa_dir);
            for (filename, _) in MESA_FILES {
                let src = exe_dir.join(filename);
                let dst = app_mesa_dir.join(filename);
                if !dst.exists() && src.exists() {
                    let _ = std::fs::copy(&src, &dst);
                }
                hide_file(&src);
            }
        }
    }
}

pub fn relaunch_self() -> ! {
    if let Ok(exe_path) = std::env::current_exe() {
        let _ = std::process::Command::new(exe_path).spawn();
    }
    std::process::exit(0);
}

fn ensure_mesa_in_exe_dir_or_relaunch() {
    let app_mesa_dir = get_mesa_dir();
    if !are_mesa_files_in(&app_mesa_dir) {
        return;
    }

    if let Some(exe_dir) = get_exe_dir() {
        if are_mesa_files_in(&exe_dir) {
            for (filename, _) in MESA_FILES {
                hide_file(&exe_dir.join(filename));
            }
            clear_crash_flag();
            return;
        }

        let mut copied_all = true;
        for (filename, _) in MESA_FILES {
            let src = app_mesa_dir.join(filename);
            let dst = exe_dir.join(filename);
            if src.exists() && !dst.exists() {
                if std::fs::copy(&src, &dst).is_err() {
                    copied_all = false;
                } else {
                    hide_file(&dst);
                }
            } else if dst.exists() {
                hide_file(&dst);
            }
        }

        if copied_all && are_mesa_files_in(&exe_dir) {
            clear_crash_flag();
            relaunch_self();
        }

        if let Ok(current_exe) = std::env::current_exe() {
            let target_exe = app_mesa_dir.join("duckdns-updater.exe");
            if current_exe != target_exe && std::fs::copy(&current_exe, &target_exe).is_ok() {
                clear_crash_flag();
                let _ = std::process::Command::new(target_exe).spawn();
                std::process::exit(0);
            }
        }
    }
}

fn get_crash_flag_path() -> std::path::PathBuf {
    core::config::AppConfig::get_config_dir().join(".gl_crash")
}

pub fn clear_crash_flag() {
    let flag = get_crash_flag_path();
    let _ = std::fs::remove_file(&flag);
}

fn setup_mesa_if_needed() -> bool {
    sync_mesa_to_appdata();
    ensure_mesa_in_exe_dir_or_relaunch();

    if let Some(exe_dir) = get_exe_dir() {
        if are_mesa_files_in(&exe_dir) {
            for (filename, _) in MESA_FILES {
                hide_file(&exe_dir.join(filename));
            }
            clear_crash_flag();
            return true;
        }
    }

    let crash_flag = get_crash_flag_path();
    if crash_flag.exists() {
        let mesa_dir = get_mesa_dir();
        match core::mesa_downloader::download_mesa_with_progress(mesa_dir.clone()) {
            Ok(()) => {
                clear_crash_flag();
                ensure_mesa_in_exe_dir_or_relaunch();
                return true;
            }
            Err(err) => {
                let msg = format!(
                    "O driver de vídeo deste computador não suporta OpenGL 2.0+ e não foi possível obter os componentes automaticamente.\n\n\
                    SOLUÇÃO MANUAL:\n\
                    1. Utilize a pasta completa 'dist' (que contém opengl32.dll, libgallium_wgl.dll e dxil.dll);\n\
                    OU\n\
                    2. Copie esses 3 arquivos para a mesma pasta do duckdns-updater.exe;\n\
                    OU\n\
                    3. Copie esses 3 arquivos para:\n{:?}\n\n\
                    Detalhe do erro: {}",
                    mesa_dir,
                    err
                );
                show_messagebox("DuckDNS Updater - Erro de Driver de Vídeo", &msg, true);
                return false;
            }
        }
    }

    let _ = std::fs::write(&crash_flag, "opengl_init");
    false
}

fn print_help() {
    println!("DuckDNS Updater v{}", env!("CARGO_PKG_VERSION"));
    println!("Atualizador nativo de IP publico para DuckDNS\n");
    println!("Uso: duckdns-updater.exe [OPCOES]\n");
    println!("Opcoes:");
    println!("  --daemon, -d      Executa em modo daemon continuo sem GUI (servidor / background)");
    println!("  --minimized       Inicia o aplicativo minimizado na bandeja do sistema");
    println!("  --cleanup-old     Remove arquivos temporarios .old de atualizacoes anteriores");
    println!("  --help, -h        Exibe esta mensagem de ajuda");
}

fn run_daemon() {
    println!("=======================================================");
    println!("  DuckDNS Updater v{} [Modo Daemon]", env!("CARGO_PKG_VERSION"));
    println!("=======================================================");
    println!("Iniciando servico em segundo plano...");

    let service = core::duckdns::DuckDnsService::new();

    loop {
        let config = core::config::AppConfig::load();
        let domains = config.domains_csv().into_owned();

        if domains.is_empty() || config.token.is_empty() {
            eprintln!(
                "[{}] AVISO: Dominio ou token nao configurados. Configure o arquivo duckdns_config.json.",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
            );
        } else if !config.update_enabled {
            println!(
                "[{}] Atualizacao automatica pausada na configuracao.",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
            );
        } else {
            let old_ipv4 = config.last_ipv4.clone();
            let old_ipv6 = config.last_ipv6.clone();
            println!(
                "[{}] Verificando IP publico para: {}...",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                domains
            );

            match service.update(&config) {
                Ok(result) => {
                    let mut cfg = config.clone();
                    let now_str = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    cfg.last_update = Some(now_str.clone());
                    cfg.last_ipv4 = result.ipv4.clone();
                    if cfg.ipv6_enabled {
                        cfg.last_ipv6 = result.ipv6.clone();
                    }
                    let _ = cfg.save();

                    println!(
                        "[{}] SUCESSO: IPv4 = {} | IPv6 = {} | Mudou? {}",
                        now_str,
                        result.ipv4.as_deref().unwrap_or("N/A"),
                        result.ipv6.as_deref().unwrap_or("N/A"),
                        if result.ip_changed { "Sim" } else { "Nao" }
                    );

                    let mut history = core::history::UpdateHistory::load();
                    history.add_entry(core::history::HistoryEntry {
                        timestamp: now_str.clone(),
                        domains: domains.clone(),
                        old_ipv4: old_ipv4.clone(),
                        new_ipv4: result.ipv4.clone(),
                        old_ipv6: old_ipv6.clone(),
                        new_ipv6: result.ipv6.clone(),
                        success: true,
                        message: "Atualizacao concluida com sucesso via daemon".to_string(),
                    });

                    if !config.notify_on_change_only || result.ip_changed {
                        let event = core::webhook::WebhookEvent {
                            domains: domains.clone(),
                            old_ipv4,
                            new_ipv4: result.ipv4,
                            old_ipv6,
                            new_ipv6: result.ipv6,
                            success: true,
                            message: "IP atualizado com sucesso pelo modo daemon".to_string(),
                            timestamp: now_str,
                        };
                        core::webhook::send_notifications(&config, &event);
                    }
                }
                Err(err) => {
                    let now_str = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    eprintln!("[{}] FALHA: {}", now_str, err);

                    let mut history = core::history::UpdateHistory::load();
                    history.add_entry(core::history::HistoryEntry {
                        timestamp: now_str.clone(),
                        domains: domains.clone(),
                        old_ipv4: old_ipv4.clone(),
                        new_ipv4: None,
                        old_ipv6: old_ipv6.clone(),
                        new_ipv6: None,
                        success: false,
                        message: format!("Falha no modo daemon: {}", err),
                    });

                    let event = core::webhook::WebhookEvent {
                        domains,
                        old_ipv4,
                        new_ipv4: None,
                        old_ipv6,
                        new_ipv6: None,
                        success: false,
                        message: format!("Falha no modo daemon: {}", err),
                        timestamp: now_str,
                    };
                    core::webhook::send_notifications(&config, &event);
                }
            }
        }

        let fresh_cfg = core::config::AppConfig::load();
        let sleep_secs = fresh_cfg.interval_minutes.max(1) as u64 * 60;
        println!(
            "[{}] Proxima verificacao em {} minuto(s)...",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            fresh_cfg.interval_minutes.max(1)
        );
        std::thread::sleep(std::time::Duration::from_secs(sleep_secs));
    }
}

#[cfg(target_os = "windows")]
fn attach_console_if_needed() {
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(dwProcessId: u32) -> i32;
        fn SetStdHandle(nStdHandle: u32, hHandle: isize) -> i32;
        fn CreateFileW(
            lpFileName: *const u16,
            dwDesiredAccess: u32,
            dwShareMode: u32,
            lpSecurityAttributes: *const std::ffi::c_void,
            dwCreationDisposition: u32,
            dwFlagsAndAttributes: u32,
            hTemplateFile: isize,
        ) -> isize;
    }
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFFFFFF;
    const STD_OUTPUT_HANDLE: u32 = 0xFFFFFFF5; // -11
    const STD_ERROR_HANDLE: u32 = 0xFFFFFFF4; // -12
    const GENERIC_READ: u32 = 0x80000000;
    const GENERIC_WRITE: u32 = 0x40000000;
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    const OPEN_EXISTING: u32 = 3;

    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) != 0 {
            let conout_name: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
            let handle = CreateFileW(
                conout_name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                0,
            );
            if handle != -1 {
                SetStdHandle(STD_OUTPUT_HANDLE, handle);
                SetStdHandle(STD_ERROR_HANDLE, handle);
            }
        }
    }
}

fn load_app_icon() -> egui::IconData {
    const RAW_RGBA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon_32.rgba"));
    egui::IconData {
        rgba: RAW_RGBA.to_vec(),
        width: 32,
        height: 32,
    }
}
