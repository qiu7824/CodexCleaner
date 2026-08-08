#[cfg(windows)]
mod platform {
    use std::{iter, ptr};

    use windows_sys::Win32::UI::{
        Shell::SetCurrentProcessExplicitAppUserModelID,
        WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK},
    };

    pub fn set_app_identity() {
        let app_id = wide("OpenAI.CodexCleaner.Desktop");
        unsafe {
            SetCurrentProcessExplicitAppUserModelID(app_id.as_ptr());
        }
    }

    pub fn show_fatal_error(message: &str) {
        let title = wide("Codex Cleaner · 启动失败");
        let message = wide(message);
        unsafe {
            MessageBoxW(
                ptr::null_mut(),
                message.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(iter::once(0)).collect()
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn set_app_identity() {}

    pub fn show_fatal_error(message: &str) {
        eprintln!("{message}");
    }
}

pub use platform::*;
