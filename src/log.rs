use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;

use crate::model::data_dir;

static LOG_LOCK: Mutex<()> = Mutex::new(());

pub fn log(msg: &str) {
    let _guard = LOG_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("valotraker.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 256 * 1024 {
            let _ = std::fs::rename(&path, dir.join("valotraker.log.old"));
        }
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{msg}");
    }
    #[cfg(debug_assertions)]
    eprintln!("{msg}");
}
