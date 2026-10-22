use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/icon.png");
    println!("cargo:rerun-if-changed=assets/fonts/MonacoFiraNerd-Regular.ttf");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set by Cargo");
    let out = PathBuf::from(&out_dir);

    compress_font(&out);
    generate_icon_rgba(&out);

    #[cfg(target_os = "windows")]
    compile_windows_resources(&out);
}

/// Comprime a Nerd Font (zlib nível 10) para embuti-la no executável com menos da metade do tamanho.
/// Em tempo de execução ela é descomprimida uma única vez, na criação da interface.
fn compress_font(out: &Path) {
    let font = std::fs::read("assets/fonts/MonacoFiraNerd-Regular.ttf")
        .expect("assets/fonts/MonacoFiraNerd-Regular.ttf não encontrado");
    let packed = miniz_oxide::deflate::compress_to_vec_zlib(&font, 10);
    std::fs::write(out.join("font.zlib"), packed).expect("falha ao gravar font.zlib");
}

/// Gera os bitmaps RGBA brutos do ícone: 32x32 (bandeja) e 64x64 (janela / barra de tarefas).
fn generate_icon_rgba(out: &Path) {
    let img = image::open("assets/icon.png").expect("assets/icon.png não encontrado");
    for size in [32u32, 64] {
        let rgba = img
            .resize_exact(size, size, image::imageops::FilterType::Lanczos3)
            .to_rgba8()
            .into_raw();
        std::fs::write(out.join(format!("icon_{size}.rgba")), rgba).expect("falha ao gravar icon rgba");
    }
}

#[cfg(target_os = "windows")]
fn compile_windows_resources(out: &Path) {
    let icon_path = Path::new("assets/icon.ico");
    let abs_ico = std::fs::canonicalize(icon_path).unwrap_or_else(|_| icon_path.to_path_buf());
    let abs_ico_str = abs_ico.to_string_lossy().replace('\\', "/");
    let clean_ico_path = abs_ico_str.strip_prefix("//?/").unwrap_or(&abs_ico_str);

    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "1.1.0".to_string());
    let mut version_parts = pkg_version.split('.');
    let major: u32 = version_parts.next().and_then(|v| v.parse().ok()).unwrap_or(1);
    let minor: u32 = version_parts.next().and_then(|v| v.parse().ok()).unwrap_or(1);
    let patch: u32 = version_parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);

    let rc_content = format!(
        r#"#pragma code_page(65001)
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "041604b0"
        BEGIN
            VALUE "CompanyName", "Leandro Pinheiro"
            VALUE "FileDescription", "DuckDNS Updater - Dynamic DNS Client"
            VALUE "FileVersion", "{pkg_version}.0"
            VALUE "InternalName", "duckdns-updater"
            VALUE "LegalCopyright", "Copyright © 2026 Leandro Pinheiro. MIT License."
            VALUE "OriginalFilename", "duckdns-updater.exe"
            VALUE "ProductName", "DuckDNS Updater"
            VALUE "ProductVersion", "{pkg_version}.0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x416, 1200
    END
END
1 ICON "{clean_ico_path}"
"#
    );

    let generated_rc_path = out.join("app_resource.rc");
    let _ = std::fs::write(&generated_rc_path, &rc_content);

    // Passo 1: .rc -> .res (formato binário de recursos do Windows)
    let res_path = out.join("windows.res");
    let rc_ok = try_windres(generated_rc_path.to_str().unwrap(), res_path.to_str().unwrap(), "res");

    if rc_ok {
        // Passo 2: .res -> COFF .o via llvm-cvtres (linkável pelo lld)
        let obj_path = out.join("windows_resource.o");
        let cvtres_ok = Command::new("llvm-cvtres")
            .args([
                res_path.to_str().unwrap(),
                &format!("/OUT:{}", obj_path.to_str().unwrap()),
                "/MACHINE:X64",
                "/NOLOGO",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if cvtres_ok {
            println!("cargo:rustc-link-arg={}", obj_path.to_str().unwrap());
            return;
        }

        // cvtres falhou: tenta gerar COFF direto via windres
        let coff_path = out.join("windows_coff.res");
        if try_windres(generated_rc_path.to_str().unwrap(), coff_path.to_str().unwrap(), "coff") {
            println!("cargo:rustc-link-arg={}", coff_path.to_str().unwrap());
            return;
        }
    }

    // Fallback para MSVC / rc.exe
    embed_resource::compile(generated_rc_path.to_str().unwrap(), embed_resource::NONE);
}

/// Compila um .rc usando llvm-windres / windres no formato indicado (`res` ou `coff`).
#[cfg(target_os = "windows")]
fn try_windres(rc: &str, out: &str, format: &str) -> bool {
    let winlibs_windres = r"C:\Users\Leandro\AppData\Local\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.MSVCRT_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin\windres.exe";
    let candidates = ["llvm-windres", "windres", winlibs_windres, "x86_64-w64-mingw32-windres"];
    let fmt_arg = format!("--output-format={format}");

    candidates.iter().any(|tool| {
        Command::new(tool)
            .args(["--input", rc, "--output", out, &fmt_arg, "--target=pe-x86-64"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}
