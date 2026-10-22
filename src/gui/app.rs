use crate::core::autostart;
use crate::core::config::AppConfig;
use crate::core::duckdns::DuckDnsService;
use crate::core::history::{HistoryEntry, UpdateHistory};
use chrono::Local;
use eframe::egui;
use notify_rust::Notification;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};

// ─── Messages for UI-driven updates ────────────────────────────────────────
pub enum UiUpdateMsg {
    Started,
    Finished {
        success: bool,
        message: String,
        #[allow(dead_code)]
        ip_changed: bool,
        /// IPs captured **before** saving to config, for accurate history.
        old_ipv4: Option<String>,
        old_ipv6: Option<String>,
        new_ipv4: Option<String>,
        new_ipv6: Option<String>,
    },
    /// Tray thread requests the window to be shown.
    ShowWindow,
    /// Notificação de progresso da atualização do executável
    AppUpdateProgress(crate::core::updater::UpdateStatus),
    /// Resultado da verificação de propagação DNS via DoH
    DnsCheckResult(crate::core::dns::DnsCheckResult),
    /// Resultado do teste de disparo de Webhook (Discord / Telegram)
    WebhookTestResult(Result<String, String>),
}

pub struct DuckDnsApp {
    config: AppConfig,
    /// Editable multi-domain string in the UI (comma-separated).
    domains_edit: String,
    /// Editable interval string — kept in app state to avoid allocating every frame.
    interval_edit: String,
    service: Arc<DuckDnsService>,
    status_message: String,
    is_updating: bool,
    tx: Sender<UiUpdateMsg>,
    rx: Receiver<UiUpdateMsg>,
    _tray_icon: Option<TrayIcon>,
    /// Shared flag: true = window is shown on screen.
    window_visible: Arc<AtomicBool>,
    /// Set by background threads after saving new results to disk.
    /// Writers use `Release`, reader (UI thread) uses `Acquire`.
    config_dirty: Arc<AtomicBool>,
    /// Shared flag: network connectivity status.
    network_online: Arc<AtomicBool>,

    history: UpdateHistory,
    last_update_instant: Option<Instant>,
    show_history_panel: bool,
    show_about_dialog: bool,
    success_flash_alpha: f32,
    /// Cached autostart state from Registry.
    autostart_enabled: bool,

    /// Nova versão remota disponível encontrada
    available_app_update: Arc<std::sync::Mutex<Option<crate::core::updater::RemoteVersionInfo>>>,
    /// Status do processo de auto-update
    app_update_status: crate::core::updater::UpdateStatus,
    /// Modal de notificação de atualização
    show_app_update_modal: bool,
    /// Se já exibiu o prompt de atualização inicial
    has_prompted_app_update: bool,

    /// Estado da verificação de propagação DNS via DoH
    is_checking_dns: bool,
    dns_check_result: Option<crate::core::dns::DnsCheckResult>,

    /// Estado do teste de Webhooks externos
    is_testing_webhook: bool,
    webhook_test_result: Option<Result<String, String>>,

    /// Feedback temporário de cópia para área de transferência ("Copiado!")
    copied_toast: Option<(String, Instant)>,
}

impl DuckDnsApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_winui3_theme(&cc.egui_ctx);

        let config = AppConfig::load();
        let domains_edit = if config.domains.is_empty() {
            config.domain.clone()
        } else {
            config.domains.join(", ")
        };
        let interval_edit = config.interval_minutes.to_string();

        let service = Arc::new(DuckDnsService::new());
        let (tx, rx) = channel::<UiUpdateMsg>();
        let config_dirty = Arc::new(AtomicBool::new(false));
        let window_visible = Arc::new(AtomicBool::new(!config.start_minimized));

        let (tray_icon, open_id, force_id, exit_id) = create_tray();

        // ── Tray pump thread ────────────────────────────────────────────────
        let service_tray = service.clone();
        let config_dirty_tray = config_dirty.clone();
        let window_visible_tray = window_visible.clone();
        let tx_tray = tx.clone();

        let _ = thread::Builder::new()
            .name("tray-pump".into())
            .spawn(move || {
                loop {
                    #[cfg(target_os = "windows")]
                    unsafe {
                        use crate::core::mesa_downloader::win32::{
                            DispatchMessageW, PeekMessageW, TranslateMessage, PM_REMOVE,
                        };
                        let mut msg = std::mem::zeroed();
                        while PeekMessageW(
                            &mut msg,
                            std::ptr::null_mut(),
                            0,
                            0,
                            PM_REMOVE,
                        ) != 0
                        {
                            TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }

                    while let Ok(ev) = MenuEvent::receiver().try_recv() {
                        if Some(&ev.id) == open_id.as_ref() {
                            // Signal the egui thread to show the window safely.
                            let _ = tx_tray.send(UiUpdateMsg::ShowWindow);
                        } else if Some(&ev.id) == force_id.as_ref() {
                            let svc = service_tray.clone();
                            let dirty = config_dirty_tray.clone();
                            let vis = window_visible_tray.clone();
                            let _ = thread::Builder::new()
                                .name("tray-force-update".into())
                                .spawn(move || {
                                    run_tray_update(&svc, &dirty, &vis);
                                });
                        } else if Some(&ev.id) == exit_id.as_ref() {
                            std::process::exit(0);
                        }
                    }

                    thread::sleep(Duration::from_millis(50));
                }
            });

        // ── Auto-update background thread ───────────────────────────────────
        let service_auto = service.clone();
        let config_dirty_auto = config_dirty.clone();
        let window_visible_auto = window_visible.clone();

        let _ = thread::Builder::new()
            .name("auto-update".into())
            .spawn(move || {
                loop {
                    // First load: read interval only.
                    let interval_cfg = AppConfig::load();
                    let interval = interval_cfg.interval_minutes.max(1) as u64;
                    thread::sleep(Duration::from_secs(interval * 60));

                    // Second load: get fresh config after sleeping.
                    let cfg = AppConfig::load();
                    if cfg.update_enabled
                        && !cfg.domains_csv().is_empty()
                        && !cfg.token.is_empty()
                    {
                        run_background_update(
                            &service_auto,
                            &cfg,
                            &config_dirty_auto,
                            &window_visible_auto,
                        );
                    }
                }
            });

        // ── Network status background thread ────────────────────────────────
        // The `Agent` is created once here and reused for all connectivity
        // checks, preserving its internal connection pool.
        let network_online = Arc::new(AtomicBool::new(true));
        let network_online_thread = network_online.clone();
        let _ = thread::Builder::new()
            .name("network-check".into())
            .spawn(move || {
                let agent = ureq::AgentBuilder::new()
                    .timeout_connect(Duration::from_secs(2))
                    .timeout(Duration::from_secs(3))
                    .build();
                loop {
                    let is_up = agent.get("https://www.duckdns.org").call().is_ok()
                        || agent.get("https://1.1.1.1").call().is_ok();
                    network_online_thread.store(is_up, Ordering::Relaxed);
                    thread::sleep(Duration::from_secs(10));
                }
            });

        let history = UpdateHistory::load();
        let autostart_enabled = autostart::is_autostart_enabled();

        let available_app_update = Arc::new(std::sync::Mutex::new(None));
        let available_app_update_bg = Arc::clone(&available_app_update);
        let egui_ctx_bg = cc.egui_ctx.clone();

        // Checagem assíncrona de nova versão no GitHub ao iniciar
        let _ = thread::Builder::new()
            .name("github-update-check".into())
            .spawn(move || {
                if let Some(info) = crate::core::updater::check_for_updates() {
                    let mut lock = available_app_update_bg.lock().unwrap_or_else(|e| e.into_inner());
                    *lock = Some(info);
                    egui_ctx_bg.request_repaint();
                }
            });

        let app = Self {
            config,
            domains_edit,
            interval_edit,
            service,
            status_message: "Pronto.".to_string(),
            is_updating: false,
            tx,
            rx,
            _tray_icon: tray_icon,
            window_visible,
            config_dirty,
            network_online,
            history,
            last_update_instant: None,
            show_history_panel: false,
            show_about_dialog: false,
            success_flash_alpha: 0.0,
            autostart_enabled,
            available_app_update,
            app_update_status: crate::core::updater::UpdateStatus::Idle,
            show_app_update_modal: false,
            has_prompted_app_update: false,
            is_checking_dns: false,
            dns_check_result: None,
            is_testing_webhook: false,
            webhook_test_result: None,
            copied_toast: None,
        };

        if app.config.update_enabled
            && !app.config.domains_csv().is_empty()
            && !app.config.token.is_empty()
        {
            app.trigger_ui_update();
        }

        app
    }

    /// Inicia o download e substituição a quente do binário do aplicativo
    fn trigger_app_auto_update(&mut self) {
        let download_url = self
            .available_app_update
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|info| info.download_url.clone());

        self.app_update_status = crate::core::updater::UpdateStatus::Downloading(0.0);
        let tx = self.tx.clone();

        let spawn_res = thread::Builder::new()
            .name("app-auto-updater".into())
            .spawn(move || {
                let tx_progress = tx.clone();
                let res = crate::core::updater::perform_auto_update(download_url, move |status| {
                    let _ = tx_progress.send(UiUpdateMsg::AppUpdateProgress(status));
                });

                if let Err(e) = res {
                    let _ = tx.send(UiUpdateMsg::AppUpdateProgress(
                        crate::core::updater::UpdateStatus::Error(e),
                    ));
                }
            });

        if let Err(e) = spawn_res {
            self.app_update_status = crate::core::updater::UpdateStatus::Error(format!(
                "Falha ao iniciar processo de atualização: {}",
                e
            ));
        }
    }

    /// Executa teste de resolução DNS via DoH em background
    fn trigger_dns_check(&mut self) {
        if self.is_checking_dns {
            return;
        }
        let domain = self
            .config
            .domains
            .first()
            .cloned()
            .unwrap_or_else(|| self.config.domain.clone());

        if domain.trim().is_empty() {
            self.dns_check_result = Some(crate::core::dns::DnsCheckResult {
                fqdn: "N/A".to_string(),
                resolved_ip: None,
                expected_ip: None,
                is_propagated: false,
                error: Some("Configure um domínio antes de testar a resolução DNS.".to_string()),
            });
            return;
        }

        self.is_checking_dns = true;
        self.dns_check_result = None;
        let expected_ip = self.config.last_ipv4.clone();
        let tx = self.tx.clone();

        let _ = thread::Builder::new()
            .name("dns-check".into())
            .spawn(move || {
                let res = crate::core::dns::check_dns_propagation(&domain, expected_ip.as_deref());
                let _ = tx.send(UiUpdateMsg::DnsCheckResult(res));
            });
    }

    /// Dispara teste de envio de Webhook (Discord / Telegram) em background
    fn trigger_webhook_test(&mut self) {
        if self.is_testing_webhook {
            return;
        }
        self.is_testing_webhook = true;
        self.webhook_test_result = None;
        let config = self.config.clone();
        let tx = self.tx.clone();

        let _ = thread::Builder::new()
            .name("webhook-test".into())
            .spawn(move || {
                let res = crate::core::webhook::test_webhook(&config);
                let _ = tx.send(UiUpdateMsg::WebhookTestResult(res));
            });
    }

    /// Trigger an update from the GUI buttons.
    fn trigger_ui_update(&self) {
        if self.is_updating {
            return;
        }

        let config = self.config.clone();
        let service = self.service.clone();
        let tx = self.tx.clone();
        let tx_thread = tx.clone();

        let _ = tx.send(UiUpdateMsg::Started);

        let spawn_res = thread::Builder::new()
            .name("ui-update".into())
            .spawn(move || {
                // Capture old IPs BEFORE the update so history is accurate.
                let old_ipv4 = config.last_ipv4.clone();
                let old_ipv6 = config.last_ipv6.clone();

                match service.update(&config) {
                    Ok(result) => {
                        let mut cfg = config.clone();
                        cfg.last_update =
                            Some(Local::now().format("%Y-%m-%d %H:%M:%S").to_string());
                        cfg.last_ipv4 = result.ipv4.clone();
                        if cfg.ipv6_enabled {
                            cfg.last_ipv6 = result.ipv6.clone();
                        }
                        let _ = cfg.save();

                        // Dispara notificações Webhook se configuradas
                        if !config.notify_on_change_only || result.ip_changed {
                            let event = crate::core::webhook::WebhookEvent {
                                domains: config.domains_csv().into_owned(),
                                old_ipv4: old_ipv4.clone(),
                                new_ipv4: result.ipv4.clone(),
                                old_ipv6: old_ipv6.clone(),
                                new_ipv6: result.ipv6.clone(),
                                success: true,
                                message: "IP público atualizado com sucesso!".to_string(),
                                timestamp: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                            };
                            let cfg_notif = config;
                            let _ = thread::Builder::new()
                                .name("webhook-ui-notify".into())
                                .spawn(move || {
                                    crate::core::webhook::send_notifications(&cfg_notif, &event);
                                });
                        }

                        let msg = format!(
                            "Atualizado! IPv4: {} | IPv6: {}",
                            result.ipv4.as_deref().unwrap_or("N/A"),
                            result.ipv6.as_deref().unwrap_or("N/A"),
                        );
                        let _ = tx_thread.send(UiUpdateMsg::Finished {
                            success: true,
                            message: msg,
                            ip_changed: result.ip_changed,
                            old_ipv4,
                            old_ipv6,
                            new_ipv4: result.ipv4,
                            new_ipv6: result.ipv6,
                        });
                    }
                    Err(err) => {
                        let event = crate::core::webhook::WebhookEvent {
                            domains: config.domains_csv().into_owned(),
                            old_ipv4: old_ipv4.clone(),
                            new_ipv4: None,
                            old_ipv6: old_ipv6.clone(),
                            new_ipv6: None,
                            success: false,
                            message: format!("Falha: {}", err),
                            timestamp: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                        };
                        let cfg_notif = config;
                        let _ = thread::Builder::new()
                            .name("webhook-ui-notify-err".into())
                            .spawn(move || {
                                crate::core::webhook::send_notifications(&cfg_notif, &event);
                            });

                        let _ = tx_thread.send(UiUpdateMsg::Finished {
                            success: false,
                            message: format!("Falha: {}", err),
                            ip_changed: false,
                            old_ipv4,
                            old_ipv6,
                            new_ipv4: None,
                            new_ipv6: None,
                        });
                    }
                }
            });

        if let Err(e) = spawn_res {
            let _ = tx.send(UiUpdateMsg::Finished {
                success: false,
                message: format!("Falha ao iniciar thread: {}", e),
                ip_changed: false,
                old_ipv4: None,
                old_ipv6: None,
                new_ipv4: None,
                new_ipv6: None,
            });
        }
    }

    fn save_settings(&mut self) {
        // Sync domains from edit string
        self.config.domains = self
            .domains_edit
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        // Keep legacy field in sync
        self.config.domain = self.config.domains_csv().into_owned();

        // Sync interval from edit string
        if let Ok(v) = self.interval_edit.parse::<u32>() {
            self.config.interval_minutes = v;
        }

        // Handle autostart toggle
        if self.config.start_with_windows != self.autostart_enabled {
            match autostart::set_autostart(self.config.start_with_windows) {
                Ok(_) => {
                    self.autostart_enabled = self.config.start_with_windows;
                }
                Err(e) => {
                    self.status_message = format!("Erro ao configurar auto-start: {}", e);
                    self.config.start_with_windows = self.autostart_enabled;
                    return;
                }
            }
        }

        match self.config.save() {
            Ok(_) => {
                self.status_message = "Configurações salvas com sucesso!".to_string();
            }
            Err(e) => {
                self.status_message = format!("Erro ao salvar: {}", e);
            }
        }
    }

    /// Validate config fields; returns list of error messages (zero heap allocation).
    fn validate(&self) -> Vec<&'static str> {
        let mut errors = Vec::new();
        let has_domain = self.domains_edit.split(',').any(|s| !s.trim().is_empty());
        if !has_domain {
            errors.push("Domínio não pode estar vazio.");
        }
        if self.config.token.trim().is_empty() {
            errors.push("Token não pode estar vazio.");
        }
        if self.config.interval_minutes < 1 {
            errors.push("Intervalo deve ser >= 1 minuto.");
        }
        errors
    }
}

impl eframe::App for DuckDnsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Request repaint every second for countdown timer & flash animation
        ctx.request_repaint_after(Duration::from_secs(1));

        // ── Reload config if a background thread saved new data ─────────────
        // Use Acquire ordering so all writes made before the Release store in
        // the background thread are guaranteed to be visible here.
        if self.config_dirty.swap(false, Ordering::Acquire) {
            let old_ipv4 = self.config.last_ipv4.clone();
            let old_ipv6 = self.config.last_ipv6.clone();
            self.config = AppConfig::load();
            self.status_message = format!(
                "Atualizado! IPv4: {} | IPv6: {}",
                self.config.last_ipv4.as_deref().unwrap_or("N/A"),
                self.config.last_ipv6.as_deref().unwrap_or("N/A"),
            );
            self.last_update_instant = Some(Instant::now());
            self.success_flash_alpha = 1.0;

            // Record in history from background update
            let entry = HistoryEntry {
                timestamp: self.config.last_update.clone().unwrap_or_default(),
                domains: self.config.domains_csv().into_owned(),
                old_ipv4,
                new_ipv4: self.config.last_ipv4.clone(),
                old_ipv6,
                new_ipv6: self.config.last_ipv6.clone(),
                success: true,
                message: self.status_message.clone(),
            };
            self.history.add_entry(entry);
        }

        // ── Intercept close ('X') → hide via egui viewport command ─────────
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.window_visible.store(false, Ordering::Relaxed);
        }

        // ── Intercept OS minimise → hide via egui viewport command ──────────
        if ctx.input(|i| i.viewport().minimized.unwrap_or(false)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.window_visible.store(false, Ordering::Relaxed);
        }

        // ── Keyboard shortcuts ─────────────────────────────────────────────
        let ctrl_held = ctx.input(|i| i.modifiers.ctrl);
        if ctrl_held && ctx.input(|i| i.key_pressed(egui::Key::S)) {
            self.save_settings();
        }
        if ctrl_held && ctx.input(|i| i.key_pressed(egui::Key::U)) {
            self.trigger_ui_update();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.window_visible.store(false, Ordering::Relaxed);
        }

        // ── Drain UI update channel ─────────────────────────────────────────
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                UiUpdateMsg::Started => {
                    self.is_updating = true;
                    self.status_message = "Atualizando IP no DuckDNS...".to_string();
                }
                UiUpdateMsg::Finished {
                    success,
                    message,
                    ip_changed: _,
                    old_ipv4,
                    old_ipv6,
                    new_ipv4,
                    new_ipv6,
                } => {
                    self.is_updating = false;

                    // Record history with correct old vs new IPs.
                    let entry = HistoryEntry {
                        timestamp: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                        domains: self.config.domains_csv().into_owned(),
                        old_ipv4,
                        new_ipv4,
                        old_ipv6,
                        new_ipv6,
                        success,
                        message: message.clone(),
                    };
                    self.history.add_entry(entry);

                    self.status_message = message;
                    if success {
                        self.config = AppConfig::load();
                        self.last_update_instant = Some(Instant::now());
                        self.success_flash_alpha = 1.0;
                    }
                }
                UiUpdateMsg::ShowWindow => {
                    // Restore window using egui's safe viewport API — no
                    // unsafe Win32 FindWindowW required.
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    self.window_visible.store(true, Ordering::Relaxed);
                }
                UiUpdateMsg::AppUpdateProgress(status) => {
                    self.app_update_status = status;
                }
                UiUpdateMsg::DnsCheckResult(res) => {
                    self.is_checking_dns = false;
                    self.dns_check_result = Some(res);
                }
                UiUpdateMsg::WebhookTestResult(res) => {
                    self.is_testing_webhook = false;
                    self.webhook_test_result = Some(res);
                }
            }
        }

        // ── Toast de cópia temporário (expira em 2 segundos) ───────────────
        if let Some((_, instant)) = &self.copied_toast {
            if instant.elapsed() > Duration::from_secs(2) {
                self.copied_toast = None;
            }
        }

        // ── Prompt automático de atualização ────────────────────────────────
        if !self.has_prompted_app_update {
            let lock = self.available_app_update.lock().unwrap_or_else(|e| e.into_inner());
            if lock.is_some() {
                self.show_app_update_modal = true;
                self.has_prompted_app_update = true;
            }
        }

        // ── Modal de Notificação de Nova Versão ─────────────────────────────
        if self.show_app_update_modal {
            let available = self.available_app_update.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(info) = available {
                egui::Window::new("Atualização do DuckDNS Updater")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.set_width(360.0);
                        ui.vertical_centered(|ui| {
                            ui.add_space(4.0);
                            ui.heading(
                                egui::RichText::new("Nova Atualização Disponível!")
                                    .color(egui::Color32::from_rgb(0, 120, 212))
                                    .strong(),
                            );
                            ui.add_space(6.0);
                            ui.label("Uma nova versão do DuckDNS Updater foi lançada no GitHub.");
                            ui.add_space(8.0);

                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label("Versão Instalada:");
                                    ui.label(
                                        egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                                            .monospace()
                                            .weak(),
                                    );
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Nova Versão:");
                                    ui.label(
                                        egui::RichText::new(format!("v{}", info.version))
                                            .monospace()
                                            .strong()
                                            .color(egui::Color32::from_rgb(46, 204, 113)),
                                    );
                                });
                            });

                            if !info.release_notes.is_empty() {
                                ui.add_space(6.0);
                                ui.label(
                                    egui::RichText::new(&info.release_notes)
                                        .small()
                                        .italics()
                                        .color(egui::Color32::from_rgb(180, 180, 180)),
                                );
                            }

                            ui.add_space(12.0);
                            ui.horizontal(|ui| {
                                let btn_update = egui::Button::new(
                                    egui::RichText::new("Atualizar Agora")
                                        .strong()
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(0, 120, 212));

                                if ui.add(btn_update).clicked() {
                                    self.show_app_update_modal = false;
                                    self.show_about_dialog = true;
                                    self.trigger_app_auto_update();
                                }

                                if ui.button("Mais Tarde").clicked() {
                                    self.show_app_update_modal = false;
                                }
                            });
                            ui.add_space(4.0);
                        });
                    });
            }
        }

        // ── Fade success flash ──────────────────────────────────────────────
        if self.success_flash_alpha > 0.0 {
            self.success_flash_alpha = (self.success_flash_alpha - 0.02).max(0.0);
        }

        // ── About Dialog & Auto-Updater ─────────────────────────────────────
        if self.show_about_dialog {
            let available = self.available_app_update.lock().unwrap_or_else(|e| e.into_inner()).clone();

            egui::Window::new("Sobre e Atualizações")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_width(410.0);
                    egui::ScrollArea::vertical()
                        .max_height(460.0)
                        .show(ui, |ui| {
                            ui.vertical_centered(|ui| {
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("DuckDNS Updater")
                                .size(20.0)
                                .strong()
                                .color(egui::Color32::WHITE),
                        );
                        ui.label(
                            egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                                .size(13.0)
                                .color(egui::Color32::from_rgb(0, 120, 212)),
                        );
                        ui.add_space(6.0);
                        ui.label("Atualizador nativo e leve de DNS dinâmico para DuckDNS.");
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("Desenvolvido por Leandro Pinheiro")
                                .strong()
                                .color(egui::Color32::WHITE),
                        );
                        ui.label(
                            egui::RichText::new("\u{f0e7} Vibecodado com Rust + egui")
                                .size(11.0)
                                .color(egui::Color32::from_rgb(0, 120, 212)),
                        );
                        ui.add_space(6.0);
                        ui.hyperlink_to("duckdns.org", "https://www.duckdns.org");
                    });

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(6.0);

                    // ── Opções do Aplicativo ────────────────────────────────────────
                    ui.label(
                        egui::RichText::new("Opções do Aplicativo")
                            .strong()
                            .size(13.0),
                    );
                    ui.add_space(4.0);

                    ui.group(|ui| {
                        if ui
                            .checkbox(
                                &mut self.config.update_enabled,
                                "Ativar atualização automática periódica",
                            )
                            .changed()
                        {
                            let _ = self.config.save();
                        }
                        if ui
                            .checkbox(&mut self.config.ipv6_enabled, "Ativar IPv6")
                            .changed()
                        {
                            let _ = self.config.save();
                        }
                        if ui
                            .checkbox(
                                &mut self.config.start_with_windows,
                                "Iniciar com o Windows",
                            )
                            .changed()
                        {
                            if let Err(e) = autostart::set_autostart(self.config.start_with_windows) {
                                self.status_message = format!("Erro ao configurar auto-start: {}", e);
                                self.config.start_with_windows = self.autostart_enabled;
                            } else {
                                self.autostart_enabled = self.config.start_with_windows;
                                let _ = self.config.save();
                            }
                        }
                        if ui
                            .checkbox(
                                &mut self.config.start_minimized,
                                "Iniciar minimizado na tray",
                            )
                            .changed()
                        {
                            let _ = self.config.save();
                        }
                        if ui
                            .checkbox(
                                &mut self.config.notify_on_change_only,
                                "Notificar apenas quando o IP mudar",
                            )
                            .changed()
                        {
                            let _ = self.config.save();
                        }
                    });

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(6.0);

                    // ── Notificações Externas (Webhooks) ───────────────────────────
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Notificações Externas (Webhooks)")
                                .strong()
                                .size(13.0),
                        );
                        ui.label(
                            egui::RichText::new("[DPAPI] Protegido")
                                .size(10.0)
                                .color(egui::Color32::from_rgb(46, 204, 113)),
                        );
                    });
                    ui.add_space(4.0);

                    ui.group(|ui| {
                        ui.label(egui::RichText::new("Discord Webhook URL:").size(12.0));
                        let d_resp = ui.add(
                            egui::TextEdit::singleline(&mut self.config.discord_webhook)
                                .password(true)
                                .hint_text("https://discord.com/api/webhooks/..."),
                        );
                        if d_resp.changed() {
                            let _ = self.config.save();
                        }

                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("Telegram Bot Token:").size(12.0));
                        let t_resp = ui.add(
                            egui::TextEdit::singleline(&mut self.config.telegram_bot_token)
                                .password(true)
                                .hint_text("Token do bot (ex: 123456789:ABCdefGh...)"),
                        );
                        if t_resp.changed() {
                            let _ = self.config.save();
                        }

                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("Telegram Chat ID:").size(12.0));
                        let c_resp = ui.add(
                            egui::TextEdit::singleline(&mut self.config.telegram_chat_id)
                                .hint_text("Chat ID (ex: 987654321 ou -100123456789)"),
                        );
                        if c_resp.changed() {
                            let _ = self.config.save();
                        }

                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            let test_btn = egui::Button::new("Testar Webhook");
                            if ui.add_enabled(!self.is_testing_webhook, test_btn).clicked() {
                                self.trigger_webhook_test();
                            }
                            if self.is_testing_webhook {
                                ui.spinner();
                                ui.label(egui::RichText::new("Enviando...").small().weak());
                            }
                        });

                        if let Some(res) = &self.webhook_test_result {
                            ui.add_space(2.0);
                            match res {
                                Ok(msg) => {
                                    ui.label(
                                        egui::RichText::new(format!("[OK] {}", msg))
                                            .size(11.0)
                                            .color(egui::Color32::from_rgb(46, 204, 113))
                                            .strong(),
                                    );
                                }
                                Err(err) => {
                                    ui.label(
                                        egui::RichText::new(format!("[!] {}", err))
                                            .size(11.0)
                                            .color(egui::Color32::from_rgb(231, 76, 60)),
                                    );
                                }
                            }
                        }
                    });

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(6.0);

                    ui.label(
                        egui::RichText::new("Atualização do Aplicativo")
                            .strong()
                            .size(13.0),
                    );
                    ui.add_space(4.0);

                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label("Versão Instalada:");
                            ui.label(
                                egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                                    .monospace()
                                    .strong(),
                            );
                        });

                        if let Some(ref info) = available {
                            ui.horizontal(|ui| {
                                ui.label("Versão no GitHub:");
                                ui.label(
                                    egui::RichText::new(format!("v{}", info.version))
                                        .monospace()
                                        .strong()
                                        .color(egui::Color32::from_rgb(46, 204, 113)),
                                );
                            });
                        } else {
                            ui.horizontal(|ui| {
                                ui.label("Status:");
                                ui.label(
                                    egui::RichText::new("Você está na versão mais recente.")
                                        .small()
                                        .color(egui::Color32::from_rgb(46, 204, 113)),
                                );
                            });
                        }
                    });

                    ui.add_space(8.0);

                    match &self.app_update_status {
                        crate::core::updater::UpdateStatus::Idle => {
                            if let Some(ref info) = available {
                                let btn_update = egui::Button::new(
                                    egui::RichText::new(format!("Baixar e Atualizar para v{}", info.version))
                                        .strong()
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(0, 120, 212));

                                if ui.add(btn_update).clicked() {
                                    self.trigger_app_auto_update();
                                }
                            } else if ui.button("Verificar Novamente").clicked() {
                                let available_bg = Arc::clone(&self.available_app_update);
                                let egui_ctx = ctx.clone();
                                thread::spawn(move || {
                                    if let Some(info) = crate::core::updater::check_for_updates() {
                                        let mut lock = available_bg.lock().unwrap_or_else(|e| e.into_inner());
                                        *lock = Some(info);
                                    }
                                    egui_ctx.request_repaint();
                                });
                            }
                        }
                        crate::core::updater::UpdateStatus::Downloading(progress) => {
                            ui.label(format!("Baixando nova versão... ({:.0}%)", progress * 100.0));
                            ui.add(egui::ProgressBar::new(*progress).animate(true));
                        }
                        crate::core::updater::UpdateStatus::Success(msg) => {
                            ui.label(
                                egui::RichText::new(msg)
                                    .color(egui::Color32::from_rgb(46, 204, 113))
                                    .strong(),
                            );
                        }
                        crate::core::updater::UpdateStatus::Error(err) => {
                            ui.label(
                                egui::RichText::new(format!("Erro ao atualizar: {}", err))
                                    .color(egui::Color32::from_rgb(231, 76, 60))
                                    .small(),
                            );
                            ui.add_space(4.0);
                            if ui.button("Tentar Novamente").clicked() {
                                self.trigger_app_auto_update();
                            }
                        }
                    }

                    }); // end ScrollArea

                    ui.add_space(8.0);
                    ui.vertical_centered(|ui| {
                        if ui.button("Fechar").clicked() {
                            self.show_about_dialog = false;
                        }
                    });
                });
        }

        // ── GUI ─────────────────────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            // ── Header ──────────────────────────────────────────────────────
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.add_space(4.0);
                    ui.heading(
                        egui::RichText::new("DuckDNS Updater")
                            .size(18.0)
                            .strong()
                            .color(egui::Color32::WHITE),
                    );
                    ui.label(
                        egui::RichText::new("Atualizador de IP público nativo")
                            .size(11.0)
                            .color(egui::Color32::from_rgb(160, 160, 160)),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("\u{f013} Sobre")
                                    .size(12.0)
                                    .strong()
                                    .color(egui::Color32::from_rgb(180, 180, 180)),
                            )
                            .min_size(egui::vec2(32.0, 26.0)),
                        )
                        .on_hover_text("Sobre o aplicativo e opções")
                        .clicked()
                    {
                        self.show_about_dialog = !self.show_about_dialog;
                    }
                });
            });

            ui.add_space(6.0);

            // ── Scrollable area for all content ─────────────────────────────
            egui::ScrollArea::vertical().show(ui, |ui| {

            // ── Status da rede Card ─────────────────────────────────────────
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Status da rede").strong().size(13.0));
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let is_online = self.network_online.load(Ordering::Relaxed);
                            let (status_text, dot_color) = if is_online {
                                ("Conectado", egui::Color32::from_rgb(46, 204, 113))
                            } else {
                                ("Desconectado", egui::Color32::from_rgb(231, 76, 60))
                            };

                            ui.label(
                                egui::RichText::new(status_text).color(dot_color).strong(),
                            );
                            ui.add_space(4.0);

                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(12.0, 12.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().circle_filled(
                                rect.center(),
                                6.0,
                                dot_color.linear_multiply(0.35),
                            );
                            ui.painter()
                                .circle_filled(rect.center(), 4.0, dot_color);
                        },
                    );
                });
            });

            ui.add_space(6.0);

            // ── Configuração Card ───────────────────────────────────────────
            ui.group(|ui| {
                ui.label(
                    egui::RichText::new("Configuração do Serviço")
                        .strong()
                        .size(13.0),
                );
                ui.add_space(6.0);

                let validation_errors = self.validate();

                egui::Grid::new("config_grid")
                    .num_columns(2)
                    .spacing([16.0, 8.0])
                    .show(ui, |ui| {
                        // ── Domínios ────────────────────────────────────
                        ui.horizontal(|ui| {
                            ui.label("Domínios:");
                            ui.label(
                                egui::RichText::new("(?)")
                                    .small()
                                    .color(egui::Color32::from_rgb(100, 100, 100)),
                            )
                            .on_hover_text(
                                "Seus subdomínios no DuckDNS, separados por vírgula.\nExemplo: meu-servidor, outro-dominio",
                            );
                        });
                        let domain_err = validation_errors
                            .iter()
                            .any(|e| e.contains("Domínio"));
                        let domain_edit = egui::TextEdit::singleline(&mut self.domains_edit)
                            .hint_text("ex: meu-servidor, outro");
                        let resp = ui.add(domain_edit);
                        if domain_err {
                            ui.painter().rect_stroke(
                                resp.rect,
                                egui::Rounding::same(4.0),
                                egui::Stroke::new(1.5f32, egui::Color32::from_rgb(231, 76, 60)),
                            );
                        }
                        ui.end_row();

                        // ── Token ───────────────────────────────────────
                        ui.horizontal(|ui| {
                            ui.label("Token DuckDNS:");
                            ui.label(
                                egui::RichText::new("(?)")
                                    .small()
                                    .color(egui::Color32::from_rgb(100, 100, 100)),
                            )
                            .on_hover_text(
                                "Seu token de acesso da conta DuckDNS.\nEncontre em: https://www.duckdns.org",
                            );
                        });
                        let token_err = validation_errors
                            .iter()
                            .any(|e| e.contains("Token"));
                        let token_edit =
                            egui::TextEdit::singleline(&mut self.config.token)
                                .password(true)
                                .hint_text("Token de acesso secreto");
                        let resp = ui.add(token_edit);
                        if token_err {
                            ui.painter().rect_stroke(
                                resp.rect,
                                egui::Rounding::same(4.0),
                                egui::Stroke::new(1.5f32, egui::Color32::from_rgb(231, 76, 60)),
                            );
                        }
                        ui.end_row();

                        // ── Intervalo ───────────────────────────────────
                        ui.horizontal(|ui| {
                            ui.label("Intervalo (min):");
                            ui.label(
                                egui::RichText::new("(?)")
                                    .small()
                                    .color(egui::Color32::from_rgb(100, 100, 100)),
                            )
                            .on_hover_text(
                                "Intervalo em minutos entre atualizações automáticas.\nMínimo: 1 minuto.",
                            );
                        });
                        let interval_err = validation_errors
                            .iter()
                            .any(|e| e.contains("Intervalo"));
                        // `interval_edit` lives in app state — no per-frame allocation.
                        let resp = ui.add(egui::TextEdit::singleline(&mut self.interval_edit));
                        if resp.changed() {
                            if let Ok(v) = self.interval_edit.parse::<u32>() {
                                self.config.interval_minutes = v;
                            }
                        }
                        if interval_err {
                            ui.painter().rect_stroke(
                                resp.rect,
                                egui::Rounding::same(4.0),
                                egui::Stroke::new(1.5f32, egui::Color32::from_rgb(231, 76, 60)),
                            );
                        }
                        ui.end_row();
                    });

                // Show validation errors
                if !validation_errors.is_empty() {
                    ui.add_space(4.0);
                    for err in &validation_errors {
                        ui.label(
                            egui::RichText::new(format!("  [!] {}", err))
                                .size(11.0)
                                .color(egui::Color32::from_rgb(231, 76, 60)),
                        );
                    }
                }
            });

            ui.add_space(6.0);

            // ── Status e Diagnóstico Card ───────────────────────────────────
            let flash_color = if self.success_flash_alpha > 0.0 {
                Some(egui::Color32::from_rgba_unmultiplied(
                    46,
                    204,
                    113,
                    (self.success_flash_alpha * 30.0) as u8,
                ))
            } else {
                None
            };

            let frame = if let Some(fc) = flash_color {
                egui::Frame::group(ui.style()).fill(fc)
            } else {
                egui::Frame::group(ui.style())
            };

            frame.show(ui, |ui| {
                ui.label(
                    egui::RichText::new("Status e Diagnóstico")
                        .strong()
                        .size(13.0),
                );
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Última Atualização:").weak());
                    ui.label(
                        egui::RichText::new(
                            self.config
                                .last_update
                                .as_deref()
                                .unwrap_or("Nunca"),
                        )
                        .strong(),
                    );
                });

                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Último IPv4:").weak());
                    let ipv4_str = self.config.last_ipv4.as_deref().unwrap_or("N/A");
                    ui.label(egui::RichText::new(ipv4_str).monospace().strong());
                    if let Some(ip) = &self.config.last_ipv4 {
                        if ui.small_button("Copiar").on_hover_text("Copiar IPv4 para área de transferência").clicked() {
                            ui.output_mut(|o| o.copied_text = ip.clone());
                            self.copied_toast = Some(("IPv4 copiado!".to_string(), Instant::now()));
                        }
                    }
                });

                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Último IPv6:").weak());
                    let ipv6_str = self.config.last_ipv6.as_deref().unwrap_or("N/A");
                    ui.label(egui::RichText::new(ipv6_str).monospace());
                    if let Some(ip) = &self.config.last_ipv6 {
                        if ui.small_button("Copiar").on_hover_text("Copiar IPv6 para área de transferência").clicked() {
                            ui.output_mut(|o| o.copied_text = ip.clone());
                            self.copied_toast = Some(("IPv6 copiado!".to_string(), Instant::now()));
                        }
                    }
                });

                if let Some((msg, _)) = &self.copied_toast {
                    ui.label(
                        egui::RichText::new(format!("  [OK] {}", msg))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(46, 204, 113))
                            .strong(),
                    );
                }

                ui.add_space(3.0);
                ui.horizontal(|ui| {
                    let dns_btn = egui::Button::new("Testar Resolução DNS (DoH)");
                    if ui.add_enabled(!self.is_checking_dns, dns_btn).clicked() {
                        self.trigger_dns_check();
                    }
                    if self.is_checking_dns {
                        ui.spinner();
                        ui.label(egui::RichText::new("Consultando DoH...").small().weak());
                    }
                });

                if let Some(res) = &self.dns_check_result {
                    ui.add_space(2.0);
                    if let Some(err) = &res.error {
                        ui.label(
                            egui::RichText::new(format!("[!] {}", err))
                                .size(11.0)
                                .color(egui::Color32::from_rgb(231, 76, 60)),
                        );
                    } else if res.is_propagated {
                        ui.label(
                            egui::RichText::new(format!(
                                "[OK] {} -> {} (Propagado!)",
                                res.fqdn,
                                res.resolved_ip.as_deref().unwrap_or("N/A")
                            ))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(46, 204, 113))
                            .strong(),
                        );
                    } else {
                        ui.label(
                            egui::RichText::new(format!(
                                "[Aguardando] {} -> {} (Esperado: {})",
                                res.fqdn,
                                res.resolved_ip.as_deref().unwrap_or("N/A"),
                                res.expected_ip.as_deref().unwrap_or("N/A")
                            ))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(241, 196, 15)),
                        );
                    }
                }

                // ── Countdown Timer ─────────────────────────────────────
                if self.config.update_enabled {
                    ui.add_space(4.0);
                    let interval_secs =
                        self.config.interval_minutes.max(1) as u64 * 60;
                    let elapsed = self
                        .last_update_instant
                        .map(|i| i.elapsed().as_secs())
                        .unwrap_or(0);
                    let remaining = interval_secs.saturating_sub(elapsed);
                    let mins = remaining / 60;
                    let secs = remaining % 60;
                    let progress = if interval_secs > 0 {
                        1.0 - (remaining as f32 / interval_secs as f32)
                    } else {
                        0.0
                    };

                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Próxima atualização:")
                                .weak(),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "{}min {:02}s",
                                mins, secs
                            ))
                            .strong()
                            .color(egui::Color32::from_rgb(52, 152, 219)),
                        );
                    });

                    let bar =
                        egui::ProgressBar::new(progress).desired_width(ui.available_width());
                    ui.add(bar);
                }

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if self.is_updating {
                        ui.spinner();
                    }
                    let color = if self.status_message.contains("Falha")
                        || self.status_message.contains("Erro")
                    {
                        egui::Color32::from_rgb(231, 76, 60)
                    } else if self.status_message.contains("sucesso")
                        || self.status_message.contains("Atualizado")
                    {
                        egui::Color32::from_rgb(46, 204, 113)
                    } else {
                        egui::Color32::from_rgb(52, 152, 219)
                    };
                    ui.label(
                        egui::RichText::new(&self.status_message).color(color),
                    );
                });
            });

            ui.add_space(8.0);

            // ── Action Buttons ──────────────────────────────────────────────
            ui.columns(2, |columns| {
                if columns[0]
                    .add_sized(
                        [columns[0].available_width(), 32.0],
                        egui::Button::new("\u{f0c7} Salvar  (Ctrl+S)"),
                    )
                    .clicked()
                {
                    self.save_settings();
                }

                let force_btn = egui::Button::new("\u{f0e7} Atualizar  (Ctrl+U)")
                    .fill(egui::Color32::from_rgb(0, 120, 212));
                if columns[1]
                    .add_sized(
                        [columns[1].available_width(), 32.0],
                        force_btn,
                    )
                    .clicked()
                {
                    self.trigger_ui_update();
                }
            });

            ui.add_space(6.0);

            ui.columns(2, |columns| {
                if columns[0]
                    .add_sized(
                        [columns[0].available_width(), 32.0],
                        egui::Button::new("\u{f08d} Ocultar  (Esc)"),
                    )
                    .clicked()
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    self.window_visible.store(false, Ordering::Relaxed);
                }

                let hist_label = if self.show_history_panel {
                    "\u{f1da} Ocultar Histórico"
                } else {
                    "\u{f1da} Histórico"
                };
                if columns[1]
                    .add_sized(
                        [columns[1].available_width(), 32.0],
                        egui::Button::new(hist_label),
                    )
                    .clicked()
                {
                    self.show_history_panel = !self.show_history_panel;
                    if self.show_history_panel {
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(490.0, 750.0)));
                    } else {
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(490.0, 560.0)));
                    }
                }
            });

            // ── History Panel (collapsible) ─────────────────────────────────
            if self.show_history_panel {
                ui.add_space(8.0);
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Histórico de Atualizações")
                                .strong()
                                .size(13.0),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui.small_button("Exportar CSV").clicked() {
                                    match self.history.save_csv_export() {
                                        Ok(path) => {
                                            self.status_message = format!(
                                                "CSV exportado: {}",
                                                path.display()
                                            );
                                        }
                                        Err(e) => {
                                            self.status_message = e;
                                        }
                                    }
                                }
                            },
                        );
                    });
                    ui.add_space(4.0);

                    if self.history.entries.is_empty() {
                        ui.label(
                            egui::RichText::new("Nenhuma atualização registrada.")
                                .weak()
                                .italics(),
                        );
                    } else {
                        // Show last 20 entries in reverse chronological order
                        let entries: Vec<_> = self
                            .history
                            .entries
                            .iter()
                            .rev()
                            .take(20)
                            .collect();

                        egui::ScrollArea::vertical()
                            .max_height(180.0)
                            .show(ui, |ui| {
                                egui::Grid::new("history_grid")
                                    .num_columns(4)
                                    .spacing([12.0, 4.0])
                                    .striped(true)
                                    .show(ui, |ui| {
                                        // Header
                                        ui.label(
                                            egui::RichText::new("Data/Hora")
                                                .strong()
                                                .size(11.0),
                                        );
                                        ui.label(
                                            egui::RichText::new("Domínio(s)")
                                                .strong()
                                                .size(11.0),
                                        );
                                        ui.label(
                                            egui::RichText::new("IPv4")
                                                .strong()
                                                .size(11.0),
                                        );
                                        ui.label(
                                            egui::RichText::new("Status")
                                                .strong()
                                                .size(11.0),
                                        );
                                        ui.end_row();

                                        for entry in &entries {
                                            ui.label(
                                                egui::RichText::new(
                                                    &entry.timestamp,
                                                )
                                                .size(11.0),
                                            );
                                            ui.label(
                                                egui::RichText::new(
                                                    &entry.domains,
                                                )
                                                .size(11.0),
                                            );
                                            ui.label(
                                                egui::RichText::new(
                                                    entry
                                                        .new_ipv4
                                                        .as_deref()
                                                        .unwrap_or("N/A"),
                                                )
                                                .size(11.0)
                                                .monospace(),
                                            );
                                            let (icon, color) = if entry.success {
                                                (
                                                    "OK",
                                                    egui::Color32::from_rgb(46, 204, 113),
                                                )
                                            } else {
                                                (
                                                    "FALHA",
                                                    egui::Color32::from_rgb(231, 76, 60),
                                                )
                                            };
                                            ui.label(
                                                egui::RichText::new(icon)
                                                    .size(11.0)
                                                    .strong()
                                                    .color(color),
                                            );
                                            ui.end_row();
                                        }
                                    });
                            });
                    }
                });
            }

            }); // end ScrollArea
        });
    }
}

// ─── Background update logic ────────────────────────────────────────────────────

fn run_tray_update(
    service: &DuckDnsService,
    config_dirty: &AtomicBool,
    window_visible: &AtomicBool,
) {
    notify_if_hidden(
        window_visible,
        "DuckDNS Updater",
        "Atualização manual iniciada...",
    );

    let config = AppConfig::load();
    if config.domains_csv().is_empty() || config.token.is_empty() {
        notify_if_hidden(
            window_visible,
            "DuckDNS Updater",
            "Configure domínio e token primeiro.",
        );
        return;
    }

    run_background_update(service, &config, config_dirty, window_visible);
}

fn run_background_update(
    service: &DuckDnsService,
    config: &AppConfig,
    config_dirty: &AtomicBool,
    window_visible: &AtomicBool,
) {
    match service.update(config) {
        Ok(result) => {
            let mut cfg = config.clone();
            cfg.last_update =
                Some(Local::now().format("%Y-%m-%d %H:%M:%S").to_string());
            cfg.last_ipv4 = result.ipv4.clone();
            if cfg.ipv6_enabled {
                cfg.last_ipv6 = result.ipv6.clone();
            }
            let _ = cfg.save();
            // Release ordering: all writes above (save to disk) must be
            // visible to the UI thread before it reads `config_dirty`.
            config_dirty.store(true, Ordering::Release);

            if !config.notify_on_change_only || result.ip_changed {
                let body = format!(
                    "Atualizado com sucesso!\nIPv4: {}\nIPv6: {}",
                    result.ipv4.as_deref().unwrap_or("N/A"),
                    result.ipv6.as_deref().unwrap_or("N/A"),
                );
                notify_if_hidden(window_visible, "DuckDNS Updater", &body);

                let event = crate::core::webhook::WebhookEvent {
                    domains: config.domains_csv().into_owned(),
                    old_ipv4: config.last_ipv4.clone(),
                    new_ipv4: result.ipv4.clone(),
                    old_ipv6: config.last_ipv6.clone(),
                    new_ipv6: result.ipv6.clone(),
                    success: true,
                    message: "Atualização em segundo plano concluída com sucesso!".to_string(),
                    timestamp: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                };
                let cfg_clone = config.clone();
                let _ = thread::Builder::new()
                    .name("webhook-bg-notify".into())
                    .spawn(move || {
                        crate::core::webhook::send_notifications(&cfg_clone, &event);
                    });
            }
        }
        Err(err) => {
            notify_if_hidden(
                window_visible,
                "DuckDNS Updater - Erro",
                &format!("Falha na atualização: {}", err),
            );

            let event = crate::core::webhook::WebhookEvent {
                domains: config.domains_csv().into_owned(),
                old_ipv4: config.last_ipv4.clone(),
                new_ipv4: None,
                old_ipv6: config.last_ipv6.clone(),
                new_ipv6: None,
                success: false,
                message: format!("Falha na atualização em segundo plano: {}", err),
                timestamp: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            };
            let cfg_clone = config.clone();
            let _ = thread::Builder::new()
                .name("webhook-bg-notify-err".into())
                .spawn(move || {
                    crate::core::webhook::send_notifications(&cfg_clone, &event);
                });
        }
    }
}

/// Show a desktop notification ONLY when the window is hidden in the tray.
fn notify_if_hidden(window_visible: &AtomicBool, title: &str, body: &str) {
    if !window_visible.load(Ordering::Relaxed) {
        let _ = Notification::new()
            .summary(title)
            .body(body)
            .timeout(notify_rust::Timeout::Milliseconds(4000))
            .show();
    }
}

// ─── Tray icon creation ────────────────────────────────────────────────────────

fn create_tray() -> (
    Option<TrayIcon>,
    Option<tray_icon::menu::MenuId>,
    Option<tray_icon::menu::MenuId>,
    Option<tray_icon::menu::MenuId>,
) {
    const RAW_RGBA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon_32.rgba"));
    let icon = match tray_icon::Icon::from_rgba(RAW_RGBA.to_vec(), 32, 32) {
        Ok(i) => i,
        Err(_) => return (None, None, None, None),
    };

    let open_item = MenuItem::new("Abrir Configurações", true, None);
    let force_item = MenuItem::new("Forçar Atualização", true, None);
    let exit_item = MenuItem::new("Sair", true, None);

    let open_id = open_item.id().clone();
    let force_id = force_item.id().clone();
    let exit_id = exit_item.id().clone();

    let tray_menu = Menu::new();
    let _ = tray_menu.append(&open_item);
    let _ = tray_menu.append(&force_item);
    let _ = tray_menu.append(&exit_item);

    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_tooltip("DuckDNS Updater")
        .with_icon(icon)
        .build()
        .ok();

    (tray_icon, Some(open_id), Some(force_id), Some(exit_id))
}

const MONACO_FIRA_NERD_ZLIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/font.zlib"));

fn apply_winui3_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();

    let bg_color = egui::Color32::from_rgb(28, 28, 28);
    let card_bg = egui::Color32::from_rgb(39, 39, 39);
    let card_border = egui::Color32::from_rgb(52, 52, 52);
    let widget_bg = egui::Color32::from_rgb(45, 45, 45);
    let widget_hover = egui::Color32::from_rgb(58, 58, 58);
    let widget_active = egui::Color32::from_rgb(34, 34, 34);

    visuals.panel_fill = bg_color;
    visuals.window_fill = bg_color;
    visuals.extreme_bg_color = egui::Color32::from_rgb(22, 22, 22);

    visuals.widgets.noninteractive.rounding = egui::Rounding::same(8.0);
    visuals.widgets.noninteractive.bg_fill = card_bg;
    visuals.widgets.noninteractive.bg_stroke =
        egui::Stroke::new(1.0f32, card_border);

    visuals.widgets.inactive.rounding = egui::Rounding::same(6.0);
    visuals.widgets.inactive.bg_fill = widget_bg;
    visuals.widgets.inactive.bg_stroke =
        egui::Stroke::new(1.0f32, card_border);

    visuals.widgets.hovered.rounding = egui::Rounding::same(6.0);
    visuals.widgets.hovered.bg_fill = widget_hover;
    visuals.widgets.hovered.bg_stroke =
        egui::Stroke::new(1.0f32, egui::Color32::from_rgb(75, 75, 75));

    visuals.widgets.active.rounding = egui::Rounding::same(6.0);
    visuals.widgets.active.bg_fill = widget_active;
    visuals.widgets.active.bg_stroke =
        egui::Stroke::new(1.0f32, egui::Color32::from_rgb(0, 120, 212));

    ctx.set_visuals(visuals);

    let mut fonts = egui::FontDefinitions::default();

    // ── Embed Monaco Fira Nerd Font diretamente no executável (descomprimido via zlib) ──
    // Garante que todos os glifos, símbolos, ícones Nerd Font e ligaduras
    // funcionem perfeitamente em qualquer computador, sem depender de fontes locais.
    let font_bytes = miniz_oxide::inflate::decompress_to_vec_zlib(MONACO_FIRA_NERD_ZLIB)
        .unwrap_or_else(|_| Vec::new());

    fonts.font_data.insert(
        "monaco_fira_nerd".to_owned(),
        egui::FontData::from_owned(font_bytes),
    );

    // Texto Proporcional padrão: Segoe UI (se disponível no Windows)
    #[cfg(target_os = "windows")]
    {
        let font_path = std::path::Path::new(r"C:\Windows\Fonts\segoeui.ttf");
        if let Ok(bytes) = std::fs::read(font_path) {
            fonts.font_data.insert(
                "segoe_ui".to_owned(),
                egui::FontData::from_owned(bytes),
            );
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "segoe_ui".to_owned());
        }
    }

    // Adiciona Monaco Fira Nerd Font como fallback primário de Proportional para símbolos e ícones
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .push("monaco_fira_nerd".to_owned());

    // Define Monaco Fira Nerd Font como fonte primária para Monospace (IPs, tokens, históricos)
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "monaco_fira_nerd".to_owned());

    ctx.set_fonts(fonts);
}
