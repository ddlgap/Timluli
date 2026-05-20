use std::path::PathBuf;
use tauri::{AppHandle, Manager};

pub fn models_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("models")
}

pub fn model_dir(app: &AppHandle, id: &str) -> PathBuf {
    models_dir(app).join(id)
}

pub fn temp_dir(app: &AppHandle, id: &str) -> PathBuf {
    models_dir(app).join(format!(".tmp-{id}"))
}

/// Returns available bytes for the calling user on the volume containing `path`.
/// Returns `None` if the path does not exist or the query fails.
#[cfg(target_os = "windows")]
pub fn free_space_bytes(path: &PathBuf) -> Option<u64> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    // Ensure the directory exists so the Win32 call can resolve the volume.
    let _ = std::fs::create_dir_all(path);

    let wide: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let mut free_caller: u64 = 0;
    let result = unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(wide.as_ptr()),
            Some(&mut free_caller as *mut u64 as *mut _),
            None,
            None,
        )
    };
    result.ok().map(|_| free_caller)
}

#[cfg(not(target_os = "windows"))]
pub fn free_space_bytes(_path: &PathBuf) -> Option<u64> {
    None // disk-space check is Windows-only for now
}
