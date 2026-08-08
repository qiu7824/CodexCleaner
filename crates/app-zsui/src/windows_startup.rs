#[cfg(windows)]
mod platform {
    use std::{
        fs, iter,
        os::windows::process::CommandExt,
        path::PathBuf,
        process::{Child, Command},
        ptr, thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use windows_sys::Win32::UI::{
        Shell::SetCurrentProcessExplicitAppUserModelID,
        WindowsAndMessaging::{FindWindowW, MessageBoxW, SetWindowTextW, MB_ICONERROR, MB_OK},
    };

    const SPLASH_TITLE: &str = "Codex Cleaner · 正在准备扫描";

    pub struct StartupSplash {
        child: Child,
        window: windows_sys::Win32::Foundation::HWND,
        progress_file: PathBuf,
    }

    impl StartupSplash {
        pub fn launch() -> Option<Self> {
            let executable = std::env::current_exe().ok()?;
            let progress_root = std::env::temp_dir().join("CodexCleaner");
            fs::create_dir_all(&progress_root).ok()?;
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_millis();
            let progress_file =
                progress_root.join(format!("startup-{}-{nonce}.txt", std::process::id()));
            fs::write(&progress_file, "0\n准备扫描").ok()?;
            let child = Command::new(executable)
                .arg("--startup-splash")
                .arg(&progress_file)
                .creation_flags(0x0800_0000)
                .spawn()
                .ok()?;
            let title = wide(SPLASH_TITLE);
            let mut window = ptr::null_mut();
            for _ in 0..30 {
                window = unsafe { FindWindowW(ptr::null(), title.as_ptr()) };
                if !window.is_null() {
                    break;
                }
                thread::sleep(Duration::from_millis(40));
            }
            Some(Self {
                child,
                window,
                progress_file,
            })
        }

        pub fn update(&self, percent: u8, stage: &str) {
            let percent = percent.min(100);
            let _ = fs::write(&self.progress_file, format!("{percent}\n{stage}"));
            if self.window.is_null() {
                return;
            }
            let title = wide(&format!("Codex Cleaner · {percent}% · {stage}"));
            unsafe {
                SetWindowTextW(self.window, title.as_ptr());
            }
        }

        pub fn complete(&self, stage: &str) {
            self.update(100, stage);
            thread::sleep(Duration::from_millis(260));
        }
    }

    impl Drop for StartupSplash {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = fs::remove_file(&self.progress_file);
        }
    }

    pub fn set_app_identity() {
        let app_id = wide("OpenAI.CodexCleaner.Desktop");
        unsafe {
            SetCurrentProcessExplicitAppUserModelID(app_id.as_ptr());
        }
    }

    pub fn splash_title() -> &'static str {
        SPLASH_TITLE
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
    pub struct StartupSplash;

    impl StartupSplash {
        pub fn launch() -> Option<Self> {
            None
        }

        pub fn update(&self, _percent: u8, _stage: &str) {}

        pub fn complete(&self, _stage: &str) {}
    }

    pub fn set_app_identity() {}

    pub fn splash_title() -> &'static str {
        "Codex Cleaner · 正在准备扫描"
    }

    pub fn show_fatal_error(message: &str) {
        eprintln!("{message}");
    }
}

pub use platform::*;
