//! Windows file and URL associations. Targets reach the platform API as data.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

pub(crate) fn open(path: &Path) -> std::io::Result<()> {
    let _apartment = ComApartment::initialize()?;
    open_with(path, |file| {
        // SAFETY: file is NUL-terminated and lives through the call. All
        // optional pointers are null. No command-line parameters are passed.
        unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                std::ptr::null(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            ) as isize
        }
    })
}

struct ComApartment;

impl ComApartment {
    fn initialize() -> std::io::Result<Self> {
        // SAFETY: the reserved pointer is null. The guard balances every
        // successful initialization on this same blocking worker thread.
        let result = unsafe {
            CoInitializeEx(
                std::ptr::null(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            )
        };
        if result < 0 {
            return Err(std::io::Error::other(format!(
                "Windows opener COM initialization failed (HRESULT {result:#x})"
            )));
        }
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: this guard exists only after successful CoInitializeEx and
        // never leaves the thread that owns the shell operation.
        unsafe { CoUninitialize() };
    }
}

fn open_with(path: &Path, execute: impl FnOnce(&[u16]) -> isize) -> std::io::Result<()> {
    let path = crate::repo_id::boundary_path(path);
    let mut file: Vec<u16> = path.as_os_str().encode_wide().collect();
    if file.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "file path contains NUL",
        ));
    }
    file.push(0);
    let result = execute(&file);
    if result > 32 {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "Windows could not open {} (ShellExecute error {result})",
            path.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_links_reach_the_default_browser_with_query_parameters_intact() {
        let url = "https://github.com/affromero/gitpane/issues/77?name=space%20here&value=%25";
        open_with(Path::new(url), |target| {
            assert_eq!(
                String::from_utf16(&target[..target.len() - 1]).unwrap(),
                url
            );
            33
        })
        .unwrap();
    }

    #[test]
    fn file_association_receives_literal_unicode_and_shell_characters() {
        let path = Path::new(r"C:\repo\日本語 & %PATH% $(echo injected).txt");
        open_with(path, |file| {
            assert_eq!(file.last(), Some(&0));
            assert_eq!(
                String::from_utf16(&file[..file.len() - 1]).unwrap(),
                path.to_str().unwrap()
            );
            33
        })
        .unwrap();
    }

    #[test]
    fn platform_launch_failure_is_reported() {
        let error = open_with(Path::new(r"C:\missing.txt"), |_| 2).unwrap_err();
        assert!(error.to_string().contains("ShellExecute error 2"));
    }

    #[test]
    fn embedded_nul_is_rejected_before_launch() {
        let error = open_with(Path::new("bad\0path"), |_| {
            panic!("invalid path reached Windows")
        })
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}
