//! Allow-list validation of meeting links sent from the phone. Mirrors
//! `server/src/meeting-url.ts` rule for rule (the server validates first; this is defence in
//! depth so a compromised relay still cannot make the desktop open an arbitrary URL).
//!
//! Rules, in order; the first failure is the reported reason:
//! length (1..=2048) -> charset (printable ASCII, no backslash) -> scheme (literal `https://`,
//! case-insensitive) -> parse -> userinfo (none) -> port (none) -> host (exact, or `*.zoom.us`
//! on a label boundary; no trailing dot).
//!
//! Test vectors: `tests/fixtures/meeting-url-vectors.json` (a copy of the server's canonical file).

pub const MAX_LEN: usize = 2048;

const ALLOWED_EXACT: &[&str] = &["zoom.us", "teams.microsoft.com", "teams.live.com", "meet.google.com"];
const ALLOWED_SUFFIX: &[&str] = &["zoom.us"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingUrlError {
    Length,
    Charset,
    Scheme,
    Parse,
    Userinfo,
    Port,
    Host,
}

impl MeetingUrlError {
    pub fn as_str(self) -> &'static str {
        match self {
            MeetingUrlError::Length => "length",
            MeetingUrlError::Charset => "charset",
            MeetingUrlError::Scheme => "scheme",
            MeetingUrlError::Parse => "parse",
            MeetingUrlError::Userinfo => "userinfo",
            MeetingUrlError::Port => "port",
            MeetingUrlError::Host => "host",
        }
    }
}

impl std::fmt::Display for MeetingUrlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub fn validate_meeting_url(raw: &str) -> Result<url::Url, MeetingUrlError> {
    if raw.is_empty() || raw.len() > MAX_LEN {
        return Err(MeetingUrlError::Length);
    }
    if !raw.bytes().all(|b| (0x21..=0x7E).contains(&b) && b != b'\\') {
        return Err(MeetingUrlError::Charset);
    }
    if raw.len() < 8 || !raw.as_bytes()[..8].eq_ignore_ascii_case(b"https://") {
        return Err(MeetingUrlError::Scheme);
    }
    let parsed = url::Url::parse(raw).map_err(|_| MeetingUrlError::Parse)?;
    if parsed.scheme() != "https" {
        return Err(MeetingUrlError::Scheme);
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(MeetingUrlError::Userinfo);
    }
    if parsed.port().is_some() {
        return Err(MeetingUrlError::Port);
    }
    let host = match parsed.host_str() {
        Some(h) if !h.is_empty() => h.to_ascii_lowercase(),
        _ => return Err(MeetingUrlError::Parse),
    };
    if host_allowed(&host) {
        Ok(parsed)
    } else {
        Err(MeetingUrlError::Host)
    }
}

fn host_allowed(host: &str) -> bool {
    if host.ends_with('.') {
        return false;
    }
    if ALLOWED_EXACT.contains(&host) {
        return true;
    }
    ALLOWED_SUFFIX.iter().any(|suffix| {
        host.len() > suffix.len() + 1
            && host.ends_with(suffix)
            && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
    })
}

/// Which service a validated URL belongs to, for the "Opening {app} meeting" toast.
pub fn provider(u: &url::Url) -> &'static str {
    match u.host_str().unwrap_or("") {
        h if h == "zoom.us" || h.ends_with(".zoom.us") => "zoom",
        "teams.microsoft.com" | "teams.live.com" => "teams",
        "meet.google.com" => "meet",
        _ => "generic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VECTORS: &str = include_str!("../../tests/fixtures/meeting-url-vectors.json");

    #[test]
    fn shared_vectors_agree() {
        let doc: serde_json::Value = serde_json::from_str(VECTORS).expect("fixture parses");
        assert_eq!(doc["maxLength"].as_u64(), Some(MAX_LEN as u64));
        let vectors = doc["vectors"].as_array().expect("vectors array");
        assert!(vectors.len() >= 40, "fixture looks truncated: {} vectors", vectors.len());
        for v in vectors {
            let url = v["url"].as_str().expect("url");
            let expected_valid = v["valid"].as_bool().expect("valid");
            let result = validate_meeting_url(url);
            assert_eq!(result.is_ok(), expected_valid, "validity mismatch for {url:?} ({:?})", v["note"]);
            if let (Err(reason), Some(expected_reason)) = (result, v["reason"].as_str()) {
                assert_eq!(reason.as_str(), expected_reason, "reason mismatch for {url:?}");
            }
        }
    }

    #[test]
    fn length_boundary() {
        let prefix = "https://zoom.us/j/";
        let ok = format!("{prefix}{}", "1".repeat(MAX_LEN - prefix.len()));
        assert_eq!(ok.len(), MAX_LEN);
        assert!(validate_meeting_url(&ok).is_ok());
        let too_long = format!("{ok}1");
        assert_eq!(validate_meeting_url(&too_long), Err(MeetingUrlError::Length));
    }

    #[test]
    fn hand_cases() {
        assert_eq!(validate_meeting_url("http://zoom.us/j/1"), Err(MeetingUrlError::Scheme));
        assert_eq!(validate_meeting_url("https://zoom.us.evil.com/j/1"), Err(MeetingUrlError::Host));
        assert!(validate_meeting_url("https://us02web.zoom.us/j/123?pwd=x").is_ok());
        // Unicode lookalikes never reach the host rule.
        assert_eq!(validate_meeting_url("https://zооm.us/j/1"), Err(MeetingUrlError::Charset));
    }

    #[test]
    fn provider_labels() {
        let z = validate_meeting_url("https://us02web.zoom.us/j/1").unwrap();
        assert_eq!(provider(&z), "zoom");
        let t = validate_meeting_url("https://teams.live.com/meet/1").unwrap();
        assert_eq!(provider(&t), "teams");
        let m = validate_meeting_url("https://meet.google.com/abc-defg-hij").unwrap();
        assert_eq!(provider(&m), "meet");
    }
}
