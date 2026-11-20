#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod buttons;
mod clock;
mod controller;
mod counts;
mod fireworks;
mod hooks;
mod keyboard;
mod layout;
mod milestones;
mod overlay;
mod settings;
mod stats;
mod storage;
mod tray;
mod ui;

use eframe::egui;
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

fn is_first_instance() -> bool {
    // The handle stays open for the life of the process; that is what holds the instance.
    match unsafe { CreateMutexW(None, false, w!("Local\\tippy-taps")) } {
        Ok(_) => (unsafe { GetLastError() }) != ERROR_ALREADY_EXISTS,
        Err(_) => true,
    }
}

fn main() -> eframe::Result {
    let background = std::env::args().any(|arg| arg == "--background");
    if !is_first_instance() {
        if !background {
            tray::show_running_instance();
        }
        return Ok(());
    }

    let dir = storage::data_dir();
    let first_run = !dir.exists();
    storage::init(&dir);
    settings::init(&dir);
    overlay::sync();
    keyboard::init(&dir.join(storage::DIR_NAME));
    autostart::init(first_run);
    hooks::spawn();
    controller::spawn();
    storage::spawn_flusher();
    let (open_tx, open_rx) = std::sync::mpsc::channel();
    tray::spawn(open_tx);
    // Tray only: the egui window is created on the first Open, so nothing flashes at login.
    if background && open_rx.recv().is_err() {
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Tippy Taps")
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
                    .expect("bundled icon"),
            )
            .with_inner_size([400.0, 640.0])
            .with_min_inner_size([340.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Tippy Taps",
        options,
        Box::new(move |cc| Ok(Box::new(ui::App::new(cc)))),
    )
}
