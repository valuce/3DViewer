//! Windows file associations for the current user (HKCU\Software\Classes).
//!
//! Windows 10/11 protect the default-app choice (UserChoice is hash-signed), so linking
//! an extension adds 3DViewer to its "Open with" list, makes it the default only where
//! no working default exists yet, and registers the app's capabilities so it gets its
//! own page in Settings → Default apps, where the user can make it the default.

use anyhow::{Context, Result};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

const PROGID: &str = "3DViewer.Model";
const APP_NAME: &str = "3DViewer";
const APP_KEY: &str = r"Software\3DViewer";
const CAPS: &str = r"Software\3DViewer\Capabilities";

/// Association state of one extension.
#[derive(Clone, Debug)]
pub struct ExtState {
    pub ext: &'static str,
    /// 3DViewer (this very exe) is in the extension's "Open with" list.
    pub linked: bool,
    /// Name of the app that opens the extension on double-click, if any.
    pub default_app: Option<String>,
    /// The effective default is 3DViewer.
    pub is_default: bool,
}

/// Human-readable format name for the associations table.
pub fn format_name(ext: &str) -> &'static str {
    match ext {
        "glb" => "glTF (binary)",
        "gltf" => "glTF",
        "obj" => "Wavefront OBJ",
        "fbx" => "Autodesk FBX",
        "stl" => "STL",
        "ply" => "PLY",
        "3mf" => "3MF",
        "off" => "OFF",
        "3ds" => "3D Studio",
        "dae" => "COLLADA",
        "blend" => "Blender",
        _ => "",
    }
}

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

fn hkcu_classes() -> Result<RegKey> {
    Ok(RegKey::predef(HKEY_CURRENT_USER).create_subkey(r"Software\Classes").context("cannot open HKCU\\Software\\Classes")?.0)
}

fn is_ours(progid: &str) -> bool {
    let exe_name = exe().map(|e| e.1).unwrap_or_default();
    progid.eq_ignore_ascii_case(PROGID) || progid.eq_ignore_ascii_case(&format!(r"Applications\{exe_name}"))
}

/// Asks the shell (the same lookup Explorer does on double-click) for `what` about `.ext`.
/// Returns None when the extension has no handler.
fn assoc_query(ext: &str, what: windows_sys::Win32::UI::Shell::ASSOCSTR) -> Option<String> {
    use windows_sys::Win32::UI::Shell::{ASSOCF_INIT_IGNOREUNKNOWN, AssocQueryStringW};
    let dot: Vec<u16> = format!(".{ext}").encode_utf16().chain(Some(0)).collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: the inputs are NUL-terminated and `len` is the capacity of `buf` in characters.
    let hr = unsafe { AssocQueryStringW(ASSOCF_INIT_IGNOREUNKNOWN, what, dot.as_ptr(), verb.as_ptr(), buf.as_mut_ptr(), &mut len) };
    if hr < 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..n]);
    (!s.is_empty()).then_some(s)
}

/// The ProgID Explorer uses on double-click.
fn effective_progid(ext: &str) -> Option<String> {
    assoc_query(ext, windows_sys::Win32::UI::Shell::ASSOCSTR_PROGID)
}

/// Current state of every extension in `exts`.
pub fn query(exts: &[&'static str]) -> Vec<ExtState> {
    let ours = is_registered();
    let classes = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Classes").ok();
    exts.iter()
        .map(|&ext| {
            let linked = ours
                && classes
                    .as_ref()
                    .and_then(|c| c.open_subkey(format!(r".{ext}\OpenWithProgids")).ok())
                    .is_some_and(|k| k.get_raw_value(PROGID).is_ok());
            let is_default = effective_progid(ext).as_deref().is_some_and(is_ours);
            let default_app = if is_default {
                Some(APP_NAME.to_string())
            } else {
                assoc_query(ext, windows_sys::Win32::UI::Shell::ASSOCSTR_FRIENDLYAPPNAME)
            };
            ExtState { ext, linked, default_app, is_default }
        })
        .collect()
}

/// Links exactly `selected` (a subset of `all`) and unlinks the rest of `all`.
/// Returns how many of the selected extensions now open in 3DViewer by default.
pub fn apply(selected: &[&str], all: &[&str]) -> Result<usize> {
    let removed: Vec<&str> = all.iter().filter(|e| !selected.contains(e)).copied().collect();
    unlink(&removed)?;
    if selected.is_empty() {
        remove_app_keys()?;
        notify_shell();
        return Ok(0);
    }
    link(selected)?;
    notify_shell();
    Ok(selected.iter().filter(|e| effective_progid(e).as_deref().is_some_and(is_ours)).count())
}

/// CLI `--register`: links the given extensions, keeping other links as they are.
pub fn register(exts: &[&str]) -> Result<usize> {
    link(exts)?;
    notify_shell();
    Ok(exts.iter().filter(|e| effective_progid(e).as_deref().is_some_and(is_ours)).count())
}

/// CLI `--unregister`: removes every trace of 3DViewer from the associations.
pub fn unregister(all: &[&str]) -> Result<()> {
    apply(&[], all).map(|_| ())
}

fn link(exts: &[&str]) -> Result<()> {
    let (exe, exe_name) = exe()?;
    let classes = hkcu_classes()?;
    let open_cmd = format!("\"{exe}\" \"%1\"");
    let icon = format!("\"{exe}\",0");

    let (prog, _) = classes.create_subkey(PROGID)?;
    prog.set_value("", &"3D model (3DViewer)")?;
    prog.set_value("FriendlyTypeName", &"3D model")?;
    prog.create_subkey("DefaultIcon")?.0.set_value("", &icon)?;
    let (open, _) = prog.create_subkey(r"shell\open")?;
    open.set_value("FriendlyAppName", &APP_NAME)?;
    open.create_subkey("command")?.0.set_value("", &open_cmd)?;

    let (app, _) = classes.create_subkey(format!(r"Applications\{exe_name}"))?;
    app.set_value("FriendlyAppName", &APP_NAME)?;
    app.create_subkey(r"shell\open\command")?.0.set_value("", &open_cmd)?;
    let (types, _) = app.create_subkey("SupportedTypes")?;

    // Settings → Default apps → 3DViewer lists these types with a "Set default" button.
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (caps, _) = hkcu.create_subkey(CAPS)?;
    caps.set_value("ApplicationName", &APP_NAME)?;
    caps.set_value("ApplicationDescription", &env!("CARGO_PKG_DESCRIPTION"))?;
    caps.set_value("ApplicationIcon", &icon)?;
    let (fa, _) = caps.create_subkey("FileAssociations")?;
    hkcu.create_subkey(r"Software\RegisteredApplications")?.0.set_value(APP_NAME, &CAPS)?;

    for ext in exts {
        let dot = format!(".{ext}");
        types.set_value(&dot, &"")?;
        fa.set_value(&dot, &PROGID)?;
        // Become the double-click default only when nothing opens the type yet
        // (checked before joining "Open with", which would count as a handler).
        let unclaimed = effective_progid(ext).is_none();
        let (key, _) = classes.create_subkey(&dot)?;
        key.create_subkey("OpenWithProgids")?.0.set_value(PROGID, &"")?;
        if unclaimed {
            key.set_value("", &PROGID)?;
        }
    }
    Ok(())
}

fn unlink(exts: &[&str]) -> Result<()> {
    let (_, exe_name) = exe()?;
    let classes = hkcu_classes()?;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let fa = hkcu.open_subkey_with_flags(format!(r"{CAPS}\FileAssociations"), KEY_READ | KEY_WRITE).ok();
    let types = classes.open_subkey_with_flags(format!(r"Applications\{exe_name}\SupportedTypes"), KEY_READ | KEY_WRITE).ok();
    for ext in exts {
        let dot = format!(".{ext}");
        if let Ok(key) = classes.open_subkey_with_flags(&dot, KEY_READ | KEY_WRITE) {
            if let Ok(owp) = key.open_subkey_with_flags("OpenWithProgids", KEY_READ | KEY_WRITE) {
                let _ = owp.delete_value(PROGID);
            }
            if key.get_value::<String, _>("").is_ok_and(|v| v == PROGID) {
                let _ = key.delete_value("");
            }
            // Drop the keys if nothing else lives in them.
            let empty = |k: &RegKey| k.query_info().is_ok_and(|i| i.sub_keys == 0 && i.values == 0);
            if key.open_subkey("OpenWithProgids").is_ok_and(|k| empty(&k)) {
                let _ = key.delete_subkey("OpenWithProgids");
            }
            if empty(&key) {
                let _ = classes.delete_subkey(&dot);
            }
        }
        if let Some(fa) = &fa {
            let _ = fa.delete_value(&dot);
        }
        if let Some(t) = &types {
            let _ = t.delete_value(&dot);
        }
    }
    Ok(())
}

fn remove_app_keys() -> Result<()> {
    let (_, exe_name) = exe()?;
    let classes = hkcu_classes()?;
    let _ = classes.delete_subkey_all(PROGID);
    let _ = classes.delete_subkey_all(format!(r"Applications\{exe_name}"));
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let _ = hkcu.delete_subkey_all(APP_KEY);
    if let Ok(ra) = hkcu.open_subkey_with_flags(r"Software\RegisteredApplications", KEY_READ | KEY_WRITE) {
        let _ = ra.delete_value(APP_NAME);
    }
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

/// Opens 3DViewer's own page in Settings → Default apps (or the general page when the
/// app is not registered yet).
pub fn open_default_apps_settings() {
    let page =
        if is_registered() { format!("ms-settings:defaultapps?registeredAppUser={APP_NAME}") } else { "ms-settings:defaultapps".into() };
    let _ = std::process::Command::new("explorer").arg(page).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_extension_has_a_name() {
        for e in crate::loaders::SUPPORTED_EXTENSIONS {
            assert!(!format_name(e).is_empty(), "{e}");
        }
    }

    #[test]
    fn known_handler_is_found() {
        // Every Windows install maps .txt to some editor.
        assert!(assoc_query("txt", windows_sys::Win32::UI::Shell::ASSOCSTR_FRIENDLYAPPNAME).is_some());
        assert!(assoc_query("no-such-extension-3dv", windows_sys::Win32::UI::Shell::ASSOCSTR_PROGID).is_none());
    }
}
