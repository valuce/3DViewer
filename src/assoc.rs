//! Windows file associations for the current user (HKCU\Software\Classes).
//!
//! Windows 10/11 protect the default-app choice, so registration adds 3DViewer to
//! "Open with" for every supported extension, makes it the default only where no
//! default exists yet, and offers to open the Default Apps settings page.

use anyhow::{Context, Result};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

const PROGID: &str = "3DViewer.Model";

fn exe() -> Result<(String, String)> {
    let p = std::env::current_exe().context("cannot locate the executable")?;
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "viewer3d.exe".into());
    Ok((p.display().to_string(), name))
}

fn notify_shell() {
    // SAFETY: plain Win32 call without pointers.
    unsafe {
        windows_sys::Win32::UI::Shell::SHChangeNotify(
            windows_sys::Win32::UI::Shell::SHCNE_ASSOCCHANGED as i32,
            windows_sys::Win32::UI::Shell::SHCNF_IDLIST,
            std::ptr::null(),
            std::ptr::null(),
        );
    }
}

pub fn register(exts: &[&str]) -> Result<usize> {
    let (exe, exe_name) = exe()?;
    let classes = RegKey::predef(HKEY_CURRENT_USER).create_subkey(r"Software\Classes")?.0;
    let open_cmd = format!("\"{exe}\" \"%1\"");

    let (prog, _) = classes.create_subkey(PROGID)?;
    prog.set_value("", &"3D model (3DViewer)")?;
    prog.create_subkey("DefaultIcon")?.0.set_value("", &format!("\"{exe}\",0"))?;
    prog.create_subkey(r"shell\open\command")?.0.set_value("", &open_cmd)?;

    let (app, _) = classes.create_subkey(format!(r"Applications\{exe_name}"))?;
    app.set_value("FriendlyAppName", &"3DViewer")?;
    app.create_subkey(r"shell\open\command")?.0.set_value("", &open_cmd)?;
    let (types, _) = app.create_subkey("SupportedTypes")?;

    let mut made_default = 0;
    for ext in exts {
        let dot = format!(".{ext}");
        types.set_value(&dot, &"")?;
        let (key, _) = classes.create_subkey(&dot)?;
        key.create_subkey("OpenWithProgids")?.0.set_value(PROGID, &"")?;
        let current: String = key.get_value("").unwrap_or_default();
        if current.is_empty() || current == PROGID {
            key.set_value("", &PROGID)?;
            made_default += 1;
        }
    }
    notify_shell();
    Ok(made_default)
}

pub fn unregister(exts: &[&str]) -> Result<()> {
    let (_, exe_name) = exe()?;
    let classes = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(r"Software\Classes", KEY_READ | KEY_WRITE)?;
    for ext in exts {
        let dot = format!(".{ext}");
        if let Ok(key) = classes.open_subkey_with_flags(&dot, KEY_READ | KEY_WRITE) {
            if let Ok(owp) = key.open_subkey_with_flags("OpenWithProgids", KEY_READ | KEY_WRITE) {
                let _ = owp.delete_value(PROGID);
            }
            let current: String = key.get_value("").unwrap_or_default();
            if current == PROGID {
                let _ = key.delete_value("");
            }
        }
    }
    let _ = classes.delete_subkey_all(PROGID);
    let _ = classes.delete_subkey_all(format!(r"Applications\{exe_name}"));
    notify_shell();
    Ok(())
}

/// True when the ProgID exists and opens this very executable.
pub fn is_registered() -> bool {
    let Ok((exe, _)) = exe() else { return false };
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(format!(r"Software\Classes\{PROGID}\shell\open\command"))
        .and_then(|k| k.get_value::<String, _>(""))
        .map(|v| v.contains(&exe))
        .unwrap_or(false)
}

pub fn open_default_apps_settings() {
    let _ = std::process::Command::new("explorer").arg("ms-settings:defaultapps").spawn();
}
