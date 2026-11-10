//! Finding and launching a Chromium-based browser with CamLooper's dedicated profile.

use anyhow::Context;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct Browser {
    pub name: &'static str,
    pub path: PathBuf,
}

/// Candidate executables, most preferred first. Only existence is checked (`find`).
#[cfg(target_os = "windows")]
fn candidates() -> Vec<(&'static str, PathBuf)> {
    let roots: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .collect();
    let rel: [(&str, &str); 4] = [
        ("Google Chrome", r"Google\Chrome\Application\chrome.exe"),
        ("Microsoft Edge", r"Microsoft\Edge\Application\msedge.exe"),
        ("Brave", r"BraveSoftware\Brave-Browser\Application\brave.exe"),
        ("Chromium", r"Chromium\Application\chrome.exe"),
    ];
    let mut out = Vec::new();
    for (name, r) in rel {
        for root in &roots {
            out.push((name, root.join(r)));
        }
    }
    out
}

#[cfg(target_os = "macos")]
fn candidates() -> Vec<(&'static str, PathBuf)> {
    let apps: [(&str, &str); 4] = [
        ("Google Chrome", "Google Chrome.app/Contents/MacOS/Google Chrome"),
        ("Microsoft Edge", "Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
        ("Brave", "Brave Browser.app/Contents/MacOS/Brave Browser"),
        ("Chromium", "Chromium.app/Contents/MacOS/Chromium"),
    ];
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    let mut out = Vec::new();
    for (name, rel) in apps {
        for root in &roots {
            out.push((name, root.join(rel)));
        }
    }
    out
}

#[cfg(all(unix, not(target_os = "macos")))]
fn candidates() -> Vec<(&'static str, PathBuf)> {
    let names: [(&str, &[&str]); 4] = [
        ("Google Chrome", &["google-chrome", "google-chrome-stable"]),
        ("Microsoft Edge", &["microsoft-edge", "microsoft-edge-stable"]),
        ("Brave", &["brave-browser", "brave"]),
        ("Chromium", &["chromium", "chromium-browser"]),
    ];
    let mut out = Vec::new();
    for (label, bins) in names {
        for bin in bins {
            if let Some(p) = which(bin) {
                out.push((label, p));
            }
        }
    }
    // The snap is not always on PATH for GUI-launched processes.
    out.push(("Chromium", PathBuf::from("/snap/bin/chromium")));
    out
}

#[cfg(all(unix, not(target_os = "macos")))]
fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(bin)).find(|p| p.is_file())
}

/// The browser auto-join will use, or `None` when no Chromium-based browser is installed.
pub fn find() -> Option<Browser> {
    candidates()
        .into_iter()
        .find(|(_, p)| p.is_file())
        .map(|(name, path)| Browser { name, path })
}

/// Command-line for the dedicated profile. `--remote-debugging-port=0` makes the browser pick a
/// free port and write it to `<profile>/DevToolsActivePort`; `--use-fake-ui-for-media-stream`
/// auto-accepts camera/microphone prompts. When an instance with this profile is already
/// running, the new process hands the URLs to it and exits (the port file stays valid).
pub fn launch_args(profile: &Path, urls: &[&str]) -> Vec<String> {
    let mut args = vec![
        format!("--user-data-dir={}", profile.display()),
        "--remote-debugging-port=0".to_string(),
        "--use-fake-ui-for-media-stream".to_string(),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--hide-crash-restore-bubble".to_string(),
        "--disable-session-crashed-bubble".to_string(),
        "--new-window".to_string(),
    ];
    args.extend(urls.iter().map(|u| u.to_string()));
    args
}

pub fn launch(browser: &Browser, profile: &Path, urls: &[&str]) -> anyhow::Result<()> {
    let mut cmd = Command::new(&browser.path);
    cmd.args(launch_args(profile, urls))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(DETACHED_PROCESS);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("launching {}", browser.path.display()))?;
    // Reap the launcher whenever it exits (immediately on hand-off, or when the browser closes).
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// `DevToolsActivePort` holds the port on line 1 and the browser-target path on line 2.
pub fn parse_devtools_active_port(contents: &str) -> Option<(u16, String)> {
    let mut lines = contents.lines();
    let port = lines.next()?.trim().parse::<u16>().ok()?;
    let path = lines.next().map(|s| s.trim().to_string()).unwrap_or_default();
    Some((port, path))
}

pub fn read_devtools_port(profile: &Path) -> Option<u16> {
    let contents = std::fs::read_to_string(profile.join("DevToolsActivePort")).ok()?;
    parse_devtools_active_port(&contents).map(|(port, _)| port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_port_file() {
        assert_eq!(
            parse_devtools_active_port("40227\n/devtools/browser/4164-cc9f\n"),
            Some((40227, "/devtools/browser/4164-cc9f".to_string()))
        );
        assert_eq!(parse_devtools_active_port("40227"), Some((40227, String::new())));
        assert_eq!(parse_devtools_active_port(""), None);
        assert_eq!(parse_devtools_active_port("not-a-port\n/x"), None);
    }

    #[test]
    fn launch_args_pin_profile_and_media_flags() {
        let args = launch_args(Path::new("/tmp/profile"), &["https://meet.google.com/abc-defg-hij"]);
        assert_eq!(args[0], "--user-data-dir=/tmp/profile");
        assert!(args.contains(&"--remote-debugging-port=0".to_string()));
        assert!(args.contains(&"--use-fake-ui-for-media-stream".to_string()));
        assert_eq!(args.last().unwrap(), "https://meet.google.com/abc-defg-hij");
        // `--new-window` must directly precede the URLs so they open together.
        let nw = args.iter().position(|a| a == "--new-window").unwrap();
        assert_eq!(nw, args.len() - 2);
    }

    #[test]
    fn candidates_prefer_chrome() {
        let c = candidates();
        assert!(!c.is_empty());
        // Whatever exists on the machine, the preference order starts with Chrome.
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        assert_eq!(c[0].0, "Google Chrome");
    }
}
