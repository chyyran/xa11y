use std::ffi::c_void;

use windows::Win32::Foundation::HWND;
use xa11y_core::{ElementData, Error, Result};

use super::{uia_call, WindowsProvider};

impl WindowsProvider {
    /// Resolve a native top-level window into an accessibility element.
    ///
    /// Unlike desktop-root discovery, this works for windows hosted on a
    /// non-interactive desktop. The calling thread must first attach to that
    /// desktop with `SetThreadDesktop`.
    pub fn element_from_native_window(&self, native_window: isize) -> Result<ElementData> {
        if native_window == 0 {
            return Err(Error::InvalidActionData {
                message: "native window handle must not be zero".to_owned(),
            });
        }
        let hwnd = HWND(native_window as *mut c_void);
        let element = uia_call(|| unsafe { self.automation.ElementFromHandle(hwnd) })?;
        let pid = uia_call(|| unsafe { element.CurrentProcessId() })?;
        let pid = u32::try_from(pid)
            .ok()
            .filter(|pid| *pid != 0)
            .ok_or_else(|| Error::Platform {
                code: -1,
                message: "UI Automation element has no owning process".to_owned(),
            })?;
        let element = uia_call(|| self.populate_cache(&element))?;
        Ok(self.build_element_data(&element, Some(pid)))
    }
}
