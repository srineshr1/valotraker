#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod catalog;
mod log;
mod model;
mod overlay;
mod riot;

use std::sync::atomic::AtomicIsize;
use std::sync::{Arc, Mutex};

use model::{Shared, View};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        overlay::info_box(
            "valotraker",
            "A small Valorant lobby overlay.\r\n\r\nCtrl+Shift+F8  show or hide\r\nCtrl+Shift+F7  drag to move\r\nCtrl+Shift+Q  quit\r\n\r\nThe window appears in agent select and in a match.\r\nUse Borderless or Windowed display mode.\r\n\r\n--demo  preview the overlay",
        );
        return;
    }
    let demo = args.iter().any(|arg| arg == "--demo");
    if !demo && !overlay::acquire_single_instance() {
        return;
    }
    overlay::set_dpi_aware();
    let view = if demo { model::demo_view() } else { View::waiting() };
    let shared = Arc::new(Shared {
        view: Mutex::new(view),
        hwnd: AtomicIsize::new(0),
    });
    if !demo {
        let worker = shared.clone();
        std::thread::spawn(move || riot::run(worker));
    }
    overlay::run(shared);
}
