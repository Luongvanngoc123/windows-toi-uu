#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod protection;
mod runtimes;
mod system;

fn main() -> eframe::Result {
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--diagnose") {
        app::diagnose();
        return Ok(());
    }
    // The read-only render check must run in CI without opening a UAC dialog.
    if !args.iter().any(|arg| arg == "--ui-smoke-dir") && !system::is_admin() {
        if let Err(error) = system::restart_as_admin() {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                MB_ICONINFORMATION, MB_OK, MessageBoxW,
            };
            let message: Vec<u16> = format!("Velo cần quyền quản trị để chạy.\n\n{error}")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let title: Vec<u16> = "Velo".encode_utf16().chain(Some(0)).collect();
            unsafe {
                MessageBoxW(
                    std::ptr::null_mut(),
                    message.as_ptr(),
                    title.as_ptr(),
                    MB_OK | MB_ICONINFORMATION,
                );
            }
        }
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Velo | Windows Toolkit")
            .with_inner_size([1180.0, 790.0])
            .with_min_inner_size([900.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Velo",
        options,
        Box::new(|cc| Ok(Box::new(app::Velo::new(cc)))),
    )
}
