//! Application placement and the connect surface's most recently opened server.
//! Separate from per-server layouts so even an empty connect window is remembered.

use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use super::{dto::Geometry, writer};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub last_server: Option<String>,
    pub geometry: Option<Geometry>,
    pub maximized: bool,
}

#[derive(Default)]
struct State {
    preferences: Preferences,
    generation: u64,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .get_or_init(|| {
            let preferences = smudgy_core::get_smudgy_home()
                .ok()
                .and_then(|home| std::fs::read(home.join("window-preferences.json")).ok())
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            Mutex::new(State {
                preferences,
                generation: 0,
            })
        })
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[must_use]
pub fn load() -> Preferences {
    let mut preferences = state().preferences.clone();
    preferences.geometry = preferences.geometry.map(|geometry| geometry.sanitized(0));
    preferences
}

fn update(change: impl FnOnce(&mut Preferences)) {
    let Some(writer) = writer::global() else {
        // The synthetic-session QA harness disables persistence entirely.
        return;
    };
    let mut state = state();
    let before = state.preferences.clone();
    change(&mut state.preferences);
    if before == state.preferences {
        return;
    }
    let Ok(home) = smudgy_core::get_smudgy_home() else {
        return;
    };
    match serde_json::to_vec_pretty(&state.preferences) {
        Ok(bytes) => {
            state.generation += 1;
            writer.publish(
                state.generation,
                home.join("window-preferences.json"),
                Arc::from(bytes),
                None,
            );
        }
        Err(error) => log::warn!("[workspace] cannot save window preferences: {error}"),
    }
}

pub fn remember_server(server: &str) {
    update(|preferences| preferences.last_server = Some(server.to_string()));
}

pub fn remember_window(geometry: &Geometry, maximized: bool) {
    update(|preferences| preferences.remember_window(geometry, maximized));
}

/// Save the OS's restore rectangle even when the window is maximized.
pub fn remember_normal_geometry(geometry: Geometry) {
    update(|preferences| preferences.geometry = Some(geometry.sanitized(0)));
}

/// The Windows restore rectangle survives maximizing/minimizing immediately
/// after a resize, before the next autosave poll. Smudgy's main window is
/// borderless, so its outer extent is also its requested client extent.
#[cfg(windows)]
#[must_use]
pub fn native_normal_geometry(raw: u64, scale: f32) -> Option<(Geometry, bool)> {
    use winapi::shared::windef::HWND;
    use winapi::um::winuser::{
        GetMonitorInfoW, GetWindowPlacement, MONITOR_DEFAULTTONEAREST, MONITORINFO,
        MonitorFromWindow, SW_SHOWMAXIMIZED, SW_SHOWMINIMIZED, WINDOWPLACEMENT,
        WPF_RESTORETOMAXIMIZED,
    };

    let hwnd = raw as HWND;
    // SAFETY: Both structs are plain Win32 output records. The HWND comes from
    // iced's live-window query and is used only during that window's lifetime.
    let (mut placement, mut monitor): (WINDOWPLACEMENT, MONITORINFO) =
        unsafe { (std::mem::zeroed(), std::mem::zeroed()) };
    placement.length = u32::try_from(std::mem::size_of::<WINDOWPLACEMENT>()).ok()?;
    monitor.cbSize = u32::try_from(std::mem::size_of::<MONITORINFO>()).ok()?;
    // SAFETY: Initialized output buffers of the correct size; no ownership is
    // transferred by these read-only Win32 queries.
    if unsafe { GetWindowPlacement(hwnd, &raw mut placement) } == 0
        || unsafe {
            GetMonitorInfoW(
                MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
                &raw mut monitor,
            )
        } == 0
    {
        return None;
    }
    let rect = placement.rcNormalPosition;
    // WINDOWPLACEMENT uses work-area coordinates for ordinary top-level
    // windows; iced's position uses screen coordinates. Account for taskbars
    // along the top/left before converting physical pixels to logical ones.
    #[allow(clippy::cast_precision_loss)]
    let geometry = Geometry {
        x: (rect.left + monitor.rcWork.left - monitor.rcMonitor.left) as f32 / scale,
        y: (rect.top + monitor.rcWork.top - monitor.rcMonitor.top) as f32 / scale,
        width: (rect.right - rect.left) as f32 / scale,
        height: (rect.bottom - rect.top) as f32 / scale,
        scale,
    };
    let maximized = placement.showCmd == u32::try_from(SW_SHOWMAXIMIZED).ok()?
        || (placement.showCmd == u32::try_from(SW_SHOWMINIMIZED).ok()?
            && placement.flags & WPF_RESTORETOMAXIMIZED != 0);
    Some((geometry.sanitized(0), maximized))
}

impl Preferences {
    fn remember_window(&mut self, geometry: &Geometry, maximized: bool) {
        // Never replace the restore extent with the maximized desktop extent.
        // Minimized windows can report zero size and must not replace it either.
        if !maximized && geometry.width > 0.0 && geometry.height > 0.0 {
            self.geometry = Some(geometry.clone().sanitized(0));
        }
        self.maximized = maximized;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maximizing_preserves_the_normal_placement() {
        let mut preferences = Preferences::default();
        let normal = Geometry {
            x: -800.0,
            y: 90.0,
            ..Geometry::default()
        };
        preferences.remember_window(&normal, false);
        preferences.remember_window(
            &Geometry {
                width: 3840.0,
                height: 2160.0,
                ..Geometry::default()
            },
            true,
        );
        let reloaded: Preferences =
            serde_json::from_slice(&serde_json::to_vec(&preferences).unwrap()).unwrap();
        assert_eq!(reloaded.geometry, Some(normal));
        assert!(reloaded.maximized);
    }

    #[cfg(windows)]
    #[test]
    fn native_placement_round_trips_a_hidden_borderless_window() {
        use winapi::shared::windef::HWND;
        use winapi::um::winuser::{CreateWindowExW, DestroyWindow, WS_POPUP};
        struct HiddenWindow(HWND);
        impl Drop for HiddenWindow {
            fn drop(&mut self) {
                // SAFETY: This test owns the window and destroys it on its creating thread.
                unsafe {
                    DestroyWindow(self.0);
                }
            }
        }
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        // SAFETY: STATIC is a predefined class; this creates a hidden window
        // with no parent, menu, or user pointer. No user-visible UI is opened.
        let window = HiddenWindow(unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                WS_POPUP,
                100,
                80,
                900,
                650,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        });
        assert!(!window.0.is_null());
        let (geometry, maximized) = native_normal_geometry(window.0 as u64, 1.0).unwrap();
        assert_eq!(
            geometry,
            Geometry {
                x: 100.0,
                y: 80.0,
                width: 900.0,
                height: 650.0,
                scale: 1.0
            }
        );
        assert!(!maximized);
    }

    #[test]
    fn empty_preferences_use_first_launch_defaults() {
        assert_eq!(
            serde_json::from_str::<Preferences>("{}").unwrap(),
            Preferences::default()
        );
    }
}
