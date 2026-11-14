use serde::{Deserialize, Serialize};
use slint::{PhysicalPosition, PhysicalSize, Window};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

const STATE_FILE: &str = "jkeeper-window.json";
const MAX_STATE_BYTES: u64 = 4096;
const MAX_DIMENSION: u32 = 16_000;
const MAX_COORDINATE: i32 = 100_000;

#[derive(Clone, Copy)]
struct Desktop {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

fn virtual_desktop() -> Desktop {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    unsafe {
        let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
        Desktop {
            left,
            top,
            right: left.saturating_add(GetSystemMetrics(SM_CXVIRTUALSCREEN)),
            bottom: top.saturating_add(GetSystemMetrics(SM_CYVIRTUALSCREEN)),
        }
    }
}

fn clamp_position(state: WindowState, desktop: Desktop) -> PhysicalPosition {
    let width = state.width.min(i32::MAX as u32) as i32;
    let height = state.height.min(i32::MAX as u32) as i32;
    let max_x = desktop.right.saturating_sub(width);
    let max_y = desktop.bottom.saturating_sub(height);
    let x = if max_x >= desktop.left {
        state.x.clamp(desktop.left, max_x)
    } else {
        desktop.left
    };
    let y = if max_y >= desktop.top {
        state.y.clamp(desktop.top, max_y)
    } else {
        desktop.top
    };
    PhysicalPosition::new(x, y)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl WindowState {
    pub fn path(manage_dir: &Path) -> PathBuf {
        manage_dir.join(STATE_FILE)
    }

    pub fn read(path: &Path, min_width: u32, min_height: u32) -> Result<Option<Self>, String> {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("读取窗口状态失败: {error}")),
        };
        if metadata.len() > MAX_STATE_BYTES {
            return Err("窗口状态文件过大".into());
        }
        let data: Self = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("窗口状态文件无效: {e}"))?;
        if data.width < min_width
            || data.height < min_height
            || data.width > MAX_DIMENSION
            || data.height > MAX_DIMENSION
            || data.x.unsigned_abs() > MAX_COORDINATE as u32
            || data.y.unsigned_abs() > MAX_COORDINATE as u32
        {
            return Err("窗口状态尺寸或坐标超出允许范围".into());
        }
        Ok(Some(data))
    }

    pub fn write_atomic(path: &Path, state: Self) -> Result<(), String> {
        let temp = path.with_extension(format!(
            "json.{}.{}.tmp",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let mut created = false;
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(|e| e.to_string())?;
            created = true;
            file.write_all(&serde_json::to_vec(&state).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            drop(file);
            crate::process::replace(&temp, path)
        })();
        if result.is_err() && created {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }

    pub fn current(window: &Window) -> Self {
        let position: PhysicalPosition = window.position();
        let size: PhysicalSize = window.size();
        Self {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
        }
    }

    pub fn apply(self, window: &Window) {
        window.set_size(PhysicalSize::new(self.width, self.height));
        window.set_position(clamp_position(self, virtual_desktop()));
    }

    pub fn center(window: &Window) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN,
        };
        let size = window.size();
        let (screen_width, screen_height) = unsafe {
            (
                GetSystemMetrics(SM_CXSCREEN) as u32,
                GetSystemMetrics(SM_CYSCREEN) as u32,
            )
        };
        let x = screen_width.saturating_sub(size.width) / 2;
        let y = screen_height.saturating_sub(size.height) / 2;
        window.set_position(PhysicalPosition::new(
            x.min(i32::MAX as u32) as i32,
            y.min(i32::MAX as u32) as i32,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn state_round_trips_and_rejects_bad_geometry() {
        let path =
            std::env::temp_dir().join(format!("jkeeper-window-state-{}.json", std::process::id()));
        let expected = WindowState {
            x: 120,
            y: 80,
            width: 1500,
            height: 850,
        };
        WindowState::write_atomic(&path, expected).unwrap();
        assert_eq!(WindowState::read(&path, 1480, 650).unwrap(), Some(expected));
        assert!(WindowState::read(&path, 1600, 650).is_err());
        let prefix = format!("{}.", path.file_name().unwrap().to_string_lossy());
        assert!(
            !std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .any(|entry| {
                    entry.ok().is_some_and(|entry| {
                        entry.file_name().to_string_lossy().starts_with(&prefix)
                    })
                })
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn malformed_state_is_reported_without_mutating_file() {
        let path = std::env::temp_dir().join(format!(
            "jkeeper-window-invalid-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, b"not json").unwrap();
        assert!(WindowState::read(&path, 1480, 650).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"not json");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn detects_saved_windows_outside_virtual_desktop() {
        let desktop = Desktop {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        assert_eq!(
            clamp_position(
                WindowState {
                    x: 100,
                    y: 100,
                    width: 1500,
                    height: 850
                },
                desktop
            ),
            PhysicalPosition::new(100, 100)
        );
        assert_eq!(
            clamp_position(
                WindowState {
                    x: 5000,
                    y: 4000,
                    width: 1500,
                    height: 850
                },
                desktop
            ),
            PhysicalPosition::new(420, 230)
        );
        assert_eq!(
            clamp_position(
                WindowState {
                    x: -1400,
                    y: 50,
                    width: 1500,
                    height: 850
                },
                desktop
            ),
            PhysicalPosition::new(0, 50)
        );
    }
}
