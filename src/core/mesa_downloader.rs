//! # Módulo de Download do Mesa3D com Barra de Progresso Win32 Nativa (`mesa_downloader.rs`)
//!
//! Este módulo gerencia o download das DLLs do Mesa3D (`opengl32.dll`, `libgallium_wgl.dll`, `dxil.dll`)
//! em segundo plano exibindo uma janela Win32 nativa com barra de progresso visual em tempo real.
//!
//! Por que Win32 nativo (GDI / User32)?
//! Quando o Mesa3D precisa ser baixado, o computador NÃO possui suporte a OpenGL 2.0+.
//! Portanto, o eframe/egui/OpenGL não pode ser inicializado ainda.
//! A janela Win32 nativa desenhada com GDI não depende de drivers de vídeo nem de bibliotecas externas,
//! funcionando de forma 100% confiável no Windows 7 SP1, 8, 8.1, 10 e 11.

use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const MESA_BASE_URL_PRIMARY: &str = "http://gf.zt.net.br:8181/mesa";
/// Espelhos GitHub das DLLs do Mesa3D, em ordem de prioridade
pub const MESA_BASE_URL_FALLBACKS: &[&str] = &[
    "https://github.com/leandrop2103/acesso_remoto_dist/releases/download/v1.0.0",
    "https://github.com/lpl2103/acesso_remoto_dist/releases/download/v1.0.0",
];

pub const MESA_FILES: &[(&str, &str, u64, &str)] = &[
    (
        "opengl32.dll",
        "Mesa3D OpenGL stub",
        139_264,
        "bf9453838bf1ed553eac02b38c2014f2199be417ada8850b297c65326f8cbdca",
    ),
    (
        "libgallium_wgl.dll",
        "Mesa3D Gallium (software renderer)",
        61_582_336,
        "78163c9a5fe4336424ffd935bd640dc7306f2c081d21024c8f0373e4e5d8114b",
    ),
    (
        "dxil.dll",
        "DirectX IL compiler",
        3_831_600,
        "feb57253eff0a622561e29b44cedbe86b89fc9a5bc8dc00fa2f98fafd712c2d8",
    ),
];

/// Total estimado em bytes de todos os arquivos combinados (~65.5 MB)
const TOTAL_ESTIMATED_BYTES: u64 = 139_264 + 61_582_336 + 3_831_600;

#[derive(Default)]
pub struct DownloadState {
    pub current_file: String,
    pub file_index: usize,
    pub total_files: usize,
    pub file_downloaded_bytes: u64,
    pub file_total_bytes: u64,
    pub total_downloaded_bytes: u64,
    pub overall_estimated_bytes: u64,
    pub status_text: String,
    pub finished: bool,
    pub success: bool,
    pub error_message: Option<String>,
}

#[cfg(target_os = "windows")]
#[allow(non_snake_case, non_camel_case_types, clippy::upper_case_acronyms)]
pub(crate) mod win32 {
    use super::*;
    use std::ffi::OsStr;
    use std::io::{Read, Write};
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;

    pub type HWND = *mut std::ffi::c_void;
    pub type HDC = *mut std::ffi::c_void;
    pub type HBRUSH = *mut std::ffi::c_void;
    pub type HFONT = *mut std::ffi::c_void;
    pub type HICON = *mut std::ffi::c_void;
    pub type HCURSOR = *mut std::ffi::c_void;
    pub type HBITMAP = *mut std::ffi::c_void;
    pub type HINSTANCE = *mut std::ffi::c_void;
    pub type WPARAM = usize;
    pub type LPARAM = isize;
    pub type LRESULT = isize;

    #[repr(C)]
    #[allow(non_snake_case)]
    pub struct WNDCLASSEXW {
        pub cbSize: u32,
        pub style: u32,
        pub lpfnWndProc: Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>,
        pub cbClsExtra: i32,
        pub cbWndExtra: i32,
        pub hInstance: HINSTANCE,
        pub hIcon: HICON,
        pub hCursor: HCURSOR,
        pub hbrBackground: HBRUSH,
        pub lpszMenuName: *const u16,
        pub lpszClassName: *const u16,
        pub hIconSm: HICON,
    }

    #[repr(C)]
    pub struct POINT {
        pub x: i32,
        pub y: i32,
    }

    #[repr(C)]
    #[allow(non_snake_case)]
    pub struct MSG {
        pub hwnd: HWND,
        pub message: u32,
        pub wParam: WPARAM,
        pub lParam: LPARAM,
        pub time: u32,
        pub pt: POINT,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct RECT {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    #[repr(C)]
    #[allow(non_snake_case)]
    pub struct PAINTSTRUCT {
        pub hdc: HDC,
        pub fErase: i32,
        pub rcPaint: RECT,
        pub fRestore: i32,
        pub fIncUpdate: i32,
        pub rgbReserved: [u8; 32],
    }

    const WM_DESTROY: u32 = 0x0002;
    const WM_PAINT: u32 = 0x000F;
    const WM_CLOSE: u32 = 0x0010;
    const WM_ERASEBKGND: u32 = 0x0014;
    const WM_USER_UPDATE: u32 = 0x8000 + 100;
    const WM_USER_FINISHED: u32 = 0x8000 + 101;

    const WS_OVERLAPPED: u32 = 0x00000000;
    const WS_CAPTION: u32 = 0x00C00000;
    const WS_SYSMENU: u32 = 0x00080000;
    const WS_MINIMIZEBOX: u32 = 0x00020000;
    const WS_EX_APPWINDOW: u32 = 0x00040000;

    const SW_SHOW: i32 = 5;
    const IDC_ARROW: usize = 32512;
    const IDI_APPLICATION: usize = 32512;
    const GWLP_USERDATA: i32 = -21;

    const DT_LEFT: u32 = 0x00000000;
    const DT_RIGHT: u32 = 0x00000002;
    const DT_SINGLELINE: u32 = 0x00000020;
    const SRCCOPY: u32 = 0x00CC0020;

    pub const PM_REMOVE: u32 = 0x0001;

    #[link(name = "user32")]
    extern "system" {
        pub fn PeekMessageW(
            lpMsg: *mut MSG,
            hWnd: HWND,
            wMsgFilterMin: u32,
            wMsgFilterMax: u32,
            wRemoveMsg: u32,
        ) -> i32;
        fn RegisterClassExW(lpwcx: *const WNDCLASSEXW) -> u16;
        fn UnregisterClassW(lpClassName: *const u16, hInstance: HINSTANCE) -> i32;
        fn CreateWindowExW(
            dwExStyle: u32,
            lpClassName: *const u16,
            lpWindowName: *const u16,
            dwStyle: u32,
            X: i32,
            Y: i32,
            nWidth: i32,
            nHeight: i32,
            hWndParent: HWND,
            hMenu: *mut std::ffi::c_void,
            hInstance: HINSTANCE,
            lpParam: *mut std::ffi::c_void,
        ) -> HWND;
        fn DefWindowProcW(hWnd: HWND, Msg: u32, wParam: WPARAM, lParam: LPARAM) -> LRESULT;
        fn DestroyWindow(hWnd: HWND) -> i32;
        fn ShowWindow(hWnd: HWND, nCmdShow: i32) -> i32;
        fn UpdateWindow(hWnd: HWND) -> i32;
        fn GetMessageW(lpMsg: *mut MSG, hWnd: HWND, wMsgFilterMin: u32, wMsgFilterMax: u32) -> i32;
        pub fn TranslateMessage(lpMsg: *const MSG) -> i32;
        pub fn DispatchMessageW(lpMsg: *const MSG) -> isize;
        fn PostMessageW(hWnd: HWND, Msg: u32, wParam: WPARAM, lParam: LPARAM) -> i32;
        fn PostQuitMessage(nExitCode: i32);
        fn InvalidateRect(hWnd: HWND, lpRect: *const RECT, bErase: i32) -> i32;
        fn BeginPaint(hWnd: HWND, lpPaint: *mut PAINTSTRUCT) -> HDC;
        fn EndPaint(hWnd: HWND, lpPaint: *const PAINTSTRUCT) -> i32;
        fn GetClientRect(hWnd: HWND, lpRect: *mut RECT) -> i32;
        fn GetSystemMetrics(nIndex: i32) -> i32;
        fn LoadCursorW(hInstance: HINSTANCE, lpCursorName: *const u16) -> HCURSOR;
        fn LoadIconW(hInstance: HINSTANCE, lpIconName: *const u16) -> HICON;
        fn FillRect(hDC: HDC, lprc: *const RECT, hbr: HBRUSH) -> i32;
        fn FrameRect(hDC: HDC, lprc: *const RECT, hbr: HBRUSH) -> i32;
        fn DrawTextW(
            hDC: HDC,
            lpchText: *const u16,
            cchText: i32,
            lprc: *mut RECT,
            format: u32,
        ) -> i32;
        fn SetWindowLongPtrW(hWnd: HWND, nIndex: i32, dwNewLong: isize) -> isize;
        fn GetWindowLongPtrW(hWnd: HWND, nIndex: i32) -> isize;
    }

    #[link(name = "gdi32")]
    extern "system" {
        fn CreateSolidBrush(color: u32) -> HBRUSH;
        fn DeleteObject(ho: *mut std::ffi::c_void) -> i32;
        fn SelectObject(hdc: HDC, h: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        fn CreateFontW(
            cHeight: i32,
            cWidth: i32,
            cEscapement: i32,
            cOrientation: i32,
            cWeight: i32,
            bItalic: u32,
            bUnderline: u32,
            bStrikeOut: u32,
            iCharSet: u32,
            iOutPrecision: u32,
            iClipPrecision: u32,
            iQuality: u32,
            iPitchAndFamily: u32,
            pszFaceName: *const u16,
        ) -> HFONT;
        fn SetBkMode(hdc: HDC, mode: i32) -> i32;
        fn SetTextColor(hdc: HDC, color: u32) -> u32;
        fn CreateCompatibleDC(hdc: HDC) -> HDC;
        fn CreateCompatibleBitmap(hdc: HDC, cx: i32, cy: i32) -> HBITMAP;
        fn BitBlt(
            hdc: HDC,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            hdcSrc: HDC,
            x1: i32,
            y1: i32,
            rop: u32,
        ) -> i32;
        fn DeleteDC(hdc: HDC) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(lpModuleName: *const u16) -> HINSTANCE;
    }

    #[inline]
    fn rgb(r: u8, g: u8, b: u8) -> u32 {
        (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
    }

    fn to_wide_str(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    }

    fn create_ui_font(size: i32, bold: bool) -> HFONT {
        let name = to_wide_str("Segoe UI");
        unsafe {
            CreateFontW(
                size,
                0,
                0,
                0,
                if bold { 700 } else { 400 },
                0,
                0,
                0,
                1, // DEFAULT_CHARSET
                0, // OUT_DEFAULT_PRECIS
                0, // CLIP_DEFAULT_PRECIS
                5, // CLEARTYPE_QUALITY
                0, // DEFAULT_PITCH | FF_DONTCARE
                name.as_ptr(),
            )
        }
    }

    unsafe fn draw_text_line(
        hdc: HDC,
        text: &str,
        mut rect: RECT,
        flags: u32,
        color: u32,
        font: HFONT,
    ) {
        let wide = to_wide_str(text);
        let old_font = SelectObject(hdc, font as *mut _);
        SetTextColor(hdc, color);
        DrawTextW(hdc, wide.as_ptr(), -1, &mut rect, flags);
        SelectObject(hdc, old_font);
    }

    unsafe fn draw_progress_bar(
        hdc: HDC,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        progress: f32,
        finished: bool,
    ) {
        let bar_rect = RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        };

        // 1. Fundo da barra (#E2E8F0)
        let bg_brush = CreateSolidBrush(rgb(226, 232, 240));
        FillRect(hdc, &bar_rect, bg_brush);
        DeleteObject(bg_brush as *mut _);

        // 2. Borda sutil (#CBD5E1)
        let border_brush = CreateSolidBrush(rgb(203, 213, 225));
        FrameRect(hdc, &bar_rect, border_brush);
        DeleteObject(border_brush as *mut _);

        // 3. Preenchimento de progresso
        let clamped = progress.clamp(0.0, 1.0);
        let fill_w = ((w - 2) as f32 * clamped).round() as i32;
        if fill_w > 0 {
            let fill_rect = RECT {
                left: x + 1,
                top: y + 1,
                right: x + 1 + fill_w,
                bottom: y + h - 1,
            };
            // Azul vibrante #0078D4 ou verde #16A34A se concluído
            let fill_color = if finished {
                rgb(22, 163, 74)
            } else {
                rgb(0, 120, 212)
            };
            let fill_brush = CreateSolidBrush(fill_color);
            FillRect(hdc, &fill_rect, fill_brush);
            DeleteObject(fill_brush as *mut _);
        }
    }

    struct WindowContext {
        state: Arc<Mutex<DownloadState>>,
        cancel_flag: Arc<AtomicBool>,
        title_font: HFONT,
        bold_font: HFONT,
        regular_font: HFONT,
        small_font: HFONT,
    }

    unsafe fn paint_window(hwnd: HWND, hdc: HDC, context: &WindowContext) {
        let mut client_rect = RECT::default();
        GetClientRect(hwnd, &mut client_rect);
        let width = client_rect.right - client_rect.left;
        let height = client_rect.bottom - client_rect.top;

        if width <= 0 || height <= 0 {
            return;
        }

        // Buffer duplo para eliminar qualquer cintilação (100% flicker-free)
        let mem_dc = CreateCompatibleDC(hdc);
        let mem_bmp = CreateCompatibleBitmap(hdc, width, height);
        let old_bmp = SelectObject(mem_dc, mem_bmp as *mut _);

        // Fundo da janela (#F8FAFC)
        let bg_brush = CreateSolidBrush(rgb(248, 250, 252));
        FillRect(mem_dc, &client_rect, bg_brush);
        DeleteObject(bg_brush as *mut _);

        SetBkMode(mem_dc, 1); // TRANSPARENT

        let (
            cur_file,
            file_idx,
            total_files,
            file_downloaded,
            file_total,
            total_downloaded,
            overall_total,
            finished,
            success,
        ) = {
            let st = context.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                st.current_file.clone(),
                st.file_index,
                st.total_files,
                st.file_downloaded_bytes,
                st.file_total_bytes,
                st.total_downloaded_bytes,
                st.overall_estimated_bytes,
                st.finished,
                st.success,
            )
        };

        let file_progress = if file_total > 0 {
            (file_downloaded as f32 / file_total as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let overall_progress = if overall_total > 0 {
            (total_downloaded as f32 / overall_total as f32).clamp(0.0, 1.0)
        } else {
            file_progress
        };

        let display_progress = if finished && success {
            1.0
        } else {
            overall_progress
        };

        // 1. Título principal
        let title_text = if finished && success {
            "Modo de Compatibilidade Configurado!"
        } else {
            "Configurando Modo de Compatibilidade (Mesa3D)"
        };
        draw_text_line(
            mem_dc,
            title_text,
            RECT {
                left: 28,
                top: 18,
                right: width - 28,
                bottom: 44,
            },
            DT_LEFT | DT_SINGLELINE,
            if finished && success {
                rgb(22, 163, 74)
            } else {
                rgb(15, 23, 42)
            },
            context.title_font,
        );

        // 2. Ação atual / Arquivo
        let action_text = if finished && success {
            "Todos os componentes foram instalados com sucesso.".to_string()
        } else if cur_file.is_empty() {
            "Conectando ao servidor de componentes...".to_string()
        } else {
            format!(
                "Baixando arquivo {} de {}: {}",
                file_idx, total_files, cur_file
            )
        };
        draw_text_line(
            mem_dc,
            &action_text,
            RECT {
                left: 28,
                top: 48,
                right: width - 28,
                bottom: 70,
            },
            DT_LEFT | DT_SINGLELINE,
            rgb(51, 65, 85),
            context.bold_font,
        );

        // 3. Barra de Progresso visual
        let bar_x = 28;
        let bar_y = 78;
        let bar_w = width - 56;
        let bar_h = 24;
        draw_progress_bar(
            mem_dc,
            bar_x,
            bar_y,
            bar_w,
            bar_h,
            display_progress,
            finished && success,
        );

        // 4. Detalhes de Progresso (Bytes e Percentual)
        let percent_int = (display_progress * 100.0).round() as u32;
        let file_mb = (file_downloaded as f64) / (1024.0 * 1024.0);
        let file_tot_mb = (file_total as f64) / (1024.0 * 1024.0);
        let tot_mb = (total_downloaded as f64) / (1024.0 * 1024.0);
        let tot_est_mb = (overall_total as f64) / (1024.0 * 1024.0);

        let left_stats = if finished && success {
            "100% concluído".to_string()
        } else if file_total > 0 {
            format!("Arquivo atual: {:.1} MB / {:.1} MB", file_mb, file_tot_mb)
        } else if file_downloaded > 0 {
            format!("Arquivo atual: {:.1} MB", file_mb)
        } else {
            "Iniciando download...".to_string()
        };

        let right_stats = if finished && success {
            format!("Total: {:.1} MB transferidos", tot_mb)
        } else {
            format!(
                "Total: {:.1} MB / {:.1} MB ({}%)",
                tot_mb, tot_est_mb, percent_int
            )
        };

        draw_text_line(
            mem_dc,
            &left_stats,
            RECT {
                left: 28,
                top: 110,
                right: width / 2,
                bottom: 132,
            },
            DT_LEFT | DT_SINGLELINE,
            rgb(71, 85, 105),
            context.regular_font,
        );

        draw_text_line(
            mem_dc,
            &right_stats,
            RECT {
                left: width / 2,
                top: 110,
                right: width - 28,
                bottom: 132,
            },
            DT_RIGHT | DT_SINGLELINE,
            if finished && success {
                rgb(22, 163, 74)
            } else {
                rgb(0, 120, 212)
            },
            context.bold_font,
        );

        // 5. Rodapé informativo
        let footer_text = if finished && success {
            "Iniciando DuckDNS Updater em modo de compatibilidade..."
        } else {
            "Por favor, aguarde. Este download é necessário apenas no primeiro uso."
        };
        draw_text_line(
            mem_dc,
            footer_text,
            RECT {
                left: 28,
                top: 144,
                right: width - 28,
                bottom: 166,
            },
            DT_LEFT | DT_SINGLELINE,
            rgb(148, 163, 184),
            context.small_font,
        );

        // Transfere o buffer para o HDC da janela
        BitBlt(hdc, 0, 0, width, height, mem_dc, 0, 0, SRCCOPY);

        // Limpeza dos recursos GDI
        SelectObject(mem_dc, old_bmp);
        DeleteObject(mem_bmp as *mut _);
        DeleteDC(mem_dc);
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_ERASEBKGND => 1,
            WM_USER_UPDATE => {
                InvalidateRect(hwnd, std::ptr::null(), 0);
                0
            }
            WM_USER_FINISHED => {
                DestroyWindow(hwnd);
                0
            }
            WM_CLOSE => {
                let user_data = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if user_data != 0 {
                    let context = &*(user_data as *const WindowContext);
                    context.cancel_flag.store(true, Ordering::SeqCst);
                }
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT {
                    hdc: std::ptr::null_mut(),
                    fErase: 0,
                    rcPaint: RECT::default(),
                    fRestore: 0,
                    fIncUpdate: 0,
                    rgbReserved: [0; 32],
                };
                let hdc = BeginPaint(hwnd, &mut ps);
                let user_data = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if user_data != 0 {
                    let context = &*(user_data as *const WindowContext);
                    paint_window(hwnd, hdc, context);
                }
                EndPaint(hwnd, &ps);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// Executa o download de todos os arquivos em segundo plano com atualização contínua da barra de progresso.
    fn run_download_worker(
        hwnd: HWND,
        target_dir: PathBuf,
        state: Arc<Mutex<DownloadState>>,
        cancel_flag: Arc<AtomicBool>,
    ) {
        let result: Result<(), String> = (|| {
            std::fs::create_dir_all(&target_dir)
                .map_err(|e| format!("Falha ao criar diretório Mesa3D em {:?}: {}", target_dir, e))?;

            let mut total_downloaded: u64 = 0;

            for (idx, (filename, description, estimated_size, expected_hash)) in
                MESA_FILES.iter().enumerate()
            {
                if cancel_flag.load(Ordering::SeqCst) {
                    return Err("Download cancelado pelo usuário.".to_string());
                }

                let dest = target_dir.join(filename);
                if dest.exists() {
                    if let Ok(metadata) = std::fs::metadata(&dest) {
                        if metadata.len() >= 1024 {
                            // Valida SHA-256 se o arquivo já existir localmente
                            if let Ok(existing_bytes) = std::fs::read(&dest) {
                                let mut hasher = Sha256::new();
                                hasher.update(&existing_bytes);
                                let hex_hash = format!("{:x}", hasher.finalize());
                                if hex_hash == *expected_hash {
                                    total_downloaded += metadata.len();
                                    continue;
                                }
                            }
                        }
                    }
                }

                {
                    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                    st.current_file = filename.to_string();
                    st.file_index = idx + 1;
                    st.total_files = MESA_FILES.len();
                    st.file_downloaded_bytes = 0;
                    st.file_total_bytes = *estimated_size;
                    st.overall_estimated_bytes = TOTAL_ESTIMATED_BYTES;
                    st.status_text = format!("Baixando {} ({})...", filename, description);
                }
                unsafe {
                    PostMessageW(hwnd, WM_USER_UPDATE, 0, 0);
                }

                let primary_url = format!("{}/{}", MESA_BASE_URL_PRIMARY, filename);

                // Tenta conectar no servidor primário com timeout rápido (5s)
                let (response, is_mirror) = match ureq::get(&primary_url)
                    .timeout(std::time::Duration::from_secs(5))
                    .call()
                {
                    Ok(resp) if resp.status() == 200 => (resp, false),
                    _ => {
                        {
                            let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                            st.current_file = format!("{} (Espelho GitHub)", filename);
                            st.status_text = format!(
                                "Servidor principal ocupado. Baixando {} via GitHub Mirror...",
                                filename
                            );
                        }
                        unsafe {
                            PostMessageW(hwnd, WM_USER_UPDATE, 0, 0);
                        }

                        // Percorre os espelhos GitHub em ordem até um responder
                        let mut fb_ok = None;
                        let mut last_err = String::new();
                        for base in MESA_BASE_URL_FALLBACKS {
                            let fallback_url = format!("{}/{}", base, filename);
                            match ureq::get(&fallback_url)
                                .timeout(std::time::Duration::from_secs(120))
                                .call()
                            {
                                Ok(resp) if resp.status() == 200 => {
                                    fb_ok = Some(resp);
                                    break;
                                }
                                Ok(resp) => {
                                    last_err = format!("HTTP {} ({})", resp.status(), fallback_url);
                                }
                                Err(e) => {
                                    last_err = format!("{} ({})", e, fallback_url);
                                }
                            }
                        }
                        let fb_resp = fb_ok.ok_or_else(|| {
                            format!(
                                "Falha ao conectar no servidor primário e nos espelhos GitHub: {}",
                                last_err
                            )
                        })?;
                        (fb_resp, true)
                    }
                };

                let content_length = response
                    .header("Content-Length")
                    .and_then(|val| val.parse::<u64>().ok())
                    .unwrap_or(*estimated_size);

                {
                    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                    st.file_total_bytes = content_length;
                    if is_mirror {
                        st.current_file = format!("{} (Espelho GitHub)", filename);
                    }
                }

                let tmp_file = dest.with_extension("tmp");
                let file = std::fs::File::create(&tmp_file).map_err(|e| {
                    format!("Falha ao criar arquivo temporário {:?}: {}", tmp_file, e)
                })?;
                let mut writer = std::io::BufWriter::new(file);

                let mut reader = response.into_reader();
                let mut buffer = [0u8; 65536];
                let mut file_downloaded: u64 = 0;
                let mut last_ui_update = Instant::now();
                let mut hasher = Sha256::new();

                loop {
                    if cancel_flag.load(Ordering::SeqCst) {
                        let _ = std::fs::remove_file(&tmp_file);
                        return Err("Download cancelado pelo usuário.".to_string());
                    }

                    let n = match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(bytes_read) => bytes_read,
                        Err(e) => {
                            let _ = std::fs::remove_file(&tmp_file);
                            return Err(format!("Erro de rede ao ler dados de {}: {}", filename, e));
                        }
                    };

                    writer
                        .write_all(&buffer[..n])
                        .map_err(|e| format!("Falha ao salvar dados de {}: {}", filename, e))?;

                    hasher.update(&buffer[..n]);

                    file_downloaded += n as u64;
                    total_downloaded += n as u64;

                    // Atualiza a interface gráfica a no máximo 25 quadros por segundo
                    if last_ui_update.elapsed() >= std::time::Duration::from_millis(40) {
                        last_ui_update = Instant::now();
                        {
                            let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                            st.file_downloaded_bytes = file_downloaded;
                            st.total_downloaded_bytes = total_downloaded;
                        }
                        unsafe {
                            PostMessageW(hwnd, WM_USER_UPDATE, 0, 0);
                        }
                    }
                }

                writer
                    .flush()
                    .map_err(|e| format!("Falha ao finalizar gravação de {}: {}", filename, e))?;
                drop(writer);

                // Validação de sanidade do executável PE (deve começar com "MZ")
                let mut head = [0u8; 2];
                if let Ok(mut check_file) = std::fs::File::open(&tmp_file) {
                    let head_read = check_file.read(&mut head).unwrap_or(0);
                    drop(check_file);
                    if head_read < 2 || &head != b"MZ" || file_downloaded < 1024 {
                        let _ = std::fs::remove_file(&tmp_file);
                        return Err(format!(
                            "O arquivo {} baixado é inválido ou está corrompido.",
                            filename
                        ));
                    }
                }

                // Validação criptográfica SHA-256
                let computed_hash = format!("{:x}", hasher.finalize());
                if computed_hash != *expected_hash {
                    let _ = std::fs::remove_file(&tmp_file);
                    return Err(format!(
                        "Hash SHA-256 inválido para {}. Esperado: {}, Obtido: {}",
                        filename, expected_hash, computed_hash
                    ));
                }

                // Substitui o arquivo temporário pelo definitivo
                if dest.exists() {
                    let _ = std::fs::remove_file(&dest);
                }
                std::fs::rename(&tmp_file, &dest).map_err(|e| {
                    format!("Falha ao renomear arquivo temporário para {:?}: {}", dest, e)
                })?;
            }

            Ok(())
        })();

        match result {
            Ok(()) => {
                {
                    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                    st.finished = true;
                    st.success = true;
                    st.status_text = "Download concluído com sucesso!".to_string();
                }
                unsafe {
                    PostMessageW(hwnd, WM_USER_UPDATE, 0, 0);
                }
                std::thread::sleep(std::time::Duration::from_millis(700));
                unsafe {
                    PostMessageW(hwnd, WM_USER_FINISHED, 1, 0);
                }
            }
            Err(e) => {
                {
                    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                    st.finished = true;
                    st.success = false;
                    st.error_message = Some(e);
                }
                unsafe {
                    PostMessageW(hwnd, WM_USER_FINISHED, 0, 0);
                }
            }
        }
    }

    /// Cria a janela Win32 e inicia o download com barra de progresso.
    pub fn download_mesa_with_progress_win32(target_dir: PathBuf) -> Result<(), String> {
        let state = Arc::new(Mutex::new(DownloadState::default()));
        let cancel_flag = Arc::new(AtomicBool::new(false));

        let class_name = to_wide_str("DuckDNSUpdater_MesaDownloader");
        let window_title =
            to_wide_str("DuckDNS Updater - Download de Componentes de Vídeo");

        unsafe {
            let h_instance = GetModuleHandleW(std::ptr::null());

            // Tenta carregar o ícone embutido do app (ID 1)
            #[allow(clippy::manual_dangling_ptr)]
            let mut h_icon = LoadIconW(h_instance, 1 as *const u16);
            if h_icon.is_null() {
                h_icon = LoadIconW(std::ptr::null_mut(), IDI_APPLICATION as *const u16);
            }
            let h_cursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW as *const u16);

            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: h_instance,
                hIcon: h_icon,
                hCursor: h_cursor,
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
                hIconSm: h_icon,
            };

            RegisterClassExW(&wc);

            let win_w = 520;
            let win_h = 220;
            let screen_w = GetSystemMetrics(0);
            let screen_h = GetSystemMetrics(1);
            let win_x = (screen_w - win_w) / 2;
            let win_y = (screen_h - win_h) / 2;

            let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
            let hwnd = CreateWindowExW(
                WS_EX_APPWINDOW,
                class_name.as_ptr(),
                window_title.as_ptr(),
                style,
                win_x,
                win_y,
                win_w,
                win_h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                h_instance,
                std::ptr::null_mut(),
            );

            if hwnd.is_null() {
                UnregisterClassW(class_name.as_ptr(), h_instance);
                return Err("Falha ao criar janela Win32 para barra de progresso.".to_string());
            }

            let mut context = WindowContext {
                state: Arc::clone(&state),
                cancel_flag: Arc::clone(&cancel_flag),
                title_font: create_ui_font(-17, true),
                bold_font: create_ui_font(-14, true),
                regular_font: create_ui_font(-13, false),
                small_font: create_ui_font(-12, false),
            };

            SetWindowLongPtrW(hwnd, GWLP_USERDATA, &mut context as *mut _ as isize);

            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);

            // Dispara a thread de download em segundo plano
            let worker_state = Arc::clone(&state);
            let worker_cancel = Arc::clone(&cancel_flag);
            let hwnd_raw = hwnd as usize;
            let worker_thread = std::thread::spawn(move || {
                run_download_worker(hwnd_raw as HWND, target_dir, worker_state, worker_cancel);
            });

            // Loop de mensagens Win32
            let mut msg = MSG {
                hwnd: std::ptr::null_mut(),
                message: 0,
                wParam: 0,
                lParam: 0,
                time: 0,
                pt: POINT { x: 0, y: 0 },
            };

            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            let _ = worker_thread.join();

            DeleteObject(context.title_font as *mut _);
            DeleteObject(context.bold_font as *mut _);
            DeleteObject(context.regular_font as *mut _);
            DeleteObject(context.small_font as *mut _);
            UnregisterClassW(class_name.as_ptr(), h_instance);

            let st = state.lock().unwrap_or_else(|e| e.into_inner());
            if cancel_flag.load(Ordering::SeqCst) {
                return Err("Download cancelado pelo usuário.".to_string());
            }
            if let Some(ref err) = st.error_message {
                return Err(err.clone());
            }
            if !st.success {
                return Err("O download foi interrompido de forma inesperada.".to_string());
            }

            Ok(())
        }
    }
}

/// Inicia o download dos componentes Mesa3D com interface de progresso nativa.
pub fn download_mesa_with_progress(target_dir: std::path::PathBuf) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        win32::download_mesa_with_progress_win32(target_dir)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = target_dir;
        Ok(())
    }
}
