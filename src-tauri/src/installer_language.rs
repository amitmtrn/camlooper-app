//! Reports the UI language the user chose in the Windows installer.
//!
//! Tauri's NSIS template shows MUI's language dialog when `displayLanguageSelector` is
//! enabled (see `tauri.conf.json`), preselecting the system language, and persists the
//! choice to `HKCU\Software\<publisher>\<productName>\Installer Language` as a decimal
//! LCID. Reading it lets the app open in the language picked at install time even when
//! that differs from the OS language.
//!
//! Returns `None` everywhere except Windows: the `.deb`/`.rpm` and `.dmg` have no
//! installer UI, so those platforms fall back to detecting the locale in the frontend.

/// Maps a Windows LCID to one of the app's locale codes.
///
/// Only the low 10 bits (the primary language) are considered, which collapses
/// sublanguages so that pt-BR (1046) and pt-PT (2070) both resolve to `pt`.
fn lcid_to_locale(lcid: u32) -> Option<&'static str> {
    Some(match lcid & 0x3FF {
        0x01 => "ar",
        0x04 => "zh",
        0x07 => "de",
        0x09 => "en",
        0x0A => "es",
        0x0C => "fr",
        0x0D => "he",
        0x10 => "it",
        0x11 => "ja",
        0x12 => "ko",
        0x15 => "pl",
        0x16 => "pt",
        0x19 => "ru",
        0x1E => "th",
        0x1F => "tr",
        0x21 => "id",
        0x2A => "vi",
        0x39 => "hi",
        _ => return None,
    })
}

/// Must match `bundle.publisher` and `productName` in `tauri.conf.json`; NSIS derives the
/// key from them.
#[cfg(target_os = "windows")]
const INSTALLER_KEY: windows::core::PCWSTR = windows::core::w!("Software\\CamLooper\\camlooper");

#[cfg(target_os = "windows")]
fn read_installer_lcid() -> Option<u32> {
    use windows::core::w;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    };

    let mut hkey = HKEY::default();
    // Room for a decimal LCID and its NUL; anything longer is not a value we wrote.
    let mut buf = [0u16; 16];
    let mut size = std::mem::size_of_val(&buf) as u32;

    // SAFETY: `hkey` is initialized before use and closed on every path out. The buffer
    // outlives the query, and `size` correctly describes it in bytes.
    let value = unsafe {
        if RegOpenKeyExW(HKEY_CURRENT_USER, INSTALLER_KEY, 0, KEY_READ, &mut hkey) != ERROR_SUCCESS
        {
            return None;
        }
        let status = RegQueryValueExW(
            hkey,
            w!("Installer Language"),
            None,
            None,
            Some(buf.as_mut_ptr().cast::<u8>()),
            Some(&mut size),
        );
        let _ = RegCloseKey(hkey);
        if status != ERROR_SUCCESS {
            return None;
        }
        let len = (size as usize / 2).min(buf.len());
        String::from_utf16_lossy(&buf[..len])
    };

    value.trim_end_matches('\0').trim().parse().ok()
}

#[cfg(not(target_os = "windows"))]
fn read_installer_lcid() -> Option<u32> {
    None
}

/// The locale code chosen in the installer, if there was one.
#[tauri::command]
pub fn installer_language() -> Option<&'static str> {
    read_installer_lcid().and_then(lcid_to_locale)
}

#[cfg(test)]
mod tests {
    use super::lcid_to_locale;

    #[test]
    fn maps_sublanguages_to_the_base_locale() {
        assert_eq!(lcid_to_locale(1033), Some("en")); // en-US
        assert_eq!(lcid_to_locale(2057), Some("en")); // en-GB
        assert_eq!(lcid_to_locale(1046), Some("pt")); // pt-BR
        assert_eq!(lcid_to_locale(2070), Some("pt")); // pt-PT
        assert_eq!(lcid_to_locale(1037), Some("he"));
    }

    #[test]
    fn rejects_languages_the_app_does_not_ship() {
        assert_eq!(lcid_to_locale(1043), None); // Dutch
        assert_eq!(lcid_to_locale(1053), None); // Swedish
    }
}
