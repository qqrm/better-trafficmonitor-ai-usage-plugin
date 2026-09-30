//! Read-only Codex usage polling. Never starts a CLI process or uses Windows SSPI.
use super::{CODEX_RATE_LIMIT_MAX_AGE, Sample};
use serde_json::Value;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const POLL_INTERVAL: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
const MAX_AUTH_BYTES: u64 = 64 * 1024;
const MAX_RESPONSE_BYTES: u64 = 256 * 1024;

// Intentionally not Debug: credentials must never enter diagnostics.
struct Auth {
    access_token: String,
    account_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PollError {
    Auth,
    Http(u16),
    Transport,
    Payload,
}

fn read_bounded(mut reader: impl Read, limit: u64) -> Option<String> {
    let mut text = String::new();
    reader
        .by_ref()
        .take(limit + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= limit).then_some(text)
}

fn parse_auth(text: &str) -> Option<Auth> {
    let root: Value = serde_json::from_str(text).ok()?;
    let tokens = root.get("tokens")?;
    let access_token = tokens.get("access_token")?.as_str()?.trim();
    let account_id = tokens.get("account_id")?.as_str()?.trim();
    if access_token.is_empty()
        || account_id.is_empty()
        || access_token.contains(['\r', '\n'])
        || account_id.contains(['\r', '\n'])
    {
        return None;
    }
    Some(Auth {
        access_token: access_token.to_owned(),
        account_id: account_id.to_owned(),
    })
}

fn parse_usage(text: &str, now: i64) -> Option<Sample> {
    let root: Value = serde_json::from_str(text).ok()?;
    let limits = root.get("rate_limit")?;
    for name in ["primary_window", "secondary_window"] {
        let Some(window) = limits.get(name).filter(|value| value.is_object()) else {
            continue;
        };
        let Some(seconds) = window.get("limit_window_seconds").and_then(Value::as_i64) else {
            continue;
        };
        if !(6 * 24 * 3600..=8 * 24 * 3600).contains(&seconds) {
            continue;
        }
        let used = window.get("used_percent")?.as_f64()?;
        let reset_at = window.get("reset_at")?.as_i64()?;
        if !used.is_finite() || !(0.0..=100.0).contains(&used) || reset_at <= now {
            return None;
        }
        return Some(Sample {
            timestamp: now,
            used,
            reset_at,
        });
    }
    None
}

fn new_agent() -> ureq::Agent {
    // Cargo enables ureq's rustls `tls` feature, not `native-tls`/Schannel.
    // Disallow redirects so credentials can only reach the selected origin.
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(4))
        .timeout_connect(Duration::from_secs(2))
        .redirects(0)
        .user_agent("codex-cli")
        .build()
}

fn fetch_usage(agent: &ureq::Agent, url: &str, auth: &Auth, now: i64) -> Result<Sample, PollError> {
    let response = agent
        .get(url)
        .set("Authorization", &format!("Bearer {}", auth.access_token))
        .set("ChatGPT-Account-Id", &auth.account_id)
        .set("Accept", "application/json")
        .call()
        .map_err(|error| match error {
            ureq::Error::Status(status, _) => PollError::Http(status),
            ureq::Error::Transport(_) => PollError::Transport,
        })?;
    if response.status() != 200 {
        return Err(PollError::Http(response.status()));
    }
    let body =
        read_bounded(response.into_reader(), MAX_RESPONSE_BYTES).ok_or(PollError::Payload)?;
    parse_usage(&body, now).ok_or(PollError::Payload)
}

#[derive(Default)]
struct PollState {
    next_poll: Option<Instant>,
    failures: u32,
    sample: Option<Sample>,
}

impl PollState {
    fn due(&self, now: Instant) -> bool {
        self.next_poll.is_none_or(|next| now >= next)
    }

    fn record(&mut self, result: Result<Sample, PollError>, now: Instant) {
        let delay = match result {
            Ok(sample) => {
                self.sample = Some(sample);
                self.failures = 0;
                POLL_INTERVAL
            }
            Err(error) => {
                self.failures = self.failures.saturating_add(1);
                if matches!(error, PollError::Http(401 | 403 | 429)) {
                    MAX_BACKOFF
                } else {
                    Duration::from_secs(
                        (60 * (1_u64 << self.failures.min(3))).min(MAX_BACKOFF.as_secs()),
                    )
                }
            }
        };
        self.next_poll = Some(now + delay);
    }

    fn fresh_sample(&self, now: i64) -> Option<Sample> {
        self.sample.filter(|sample| {
            now >= sample.timestamp
                && now - sample.timestamp <= CODEX_RATE_LIMIT_MAX_AGE.as_secs() as i64
                && sample.reset_at > now
        })
    }
}

pub(super) fn load_sample(auth_path: &Path, now: i64) -> Option<Sample> {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    static STATE: OnceLock<Mutex<PollState>> = OnceLock::new();
    let mut state = STATE
        .get_or_init(|| Mutex::new(PollState::default()))
        .lock()
        .ok()?;
    if state.due(Instant::now()) {
        // Re-read the existing token on every poll; Codex owns login/refresh.
        // Do not copy it, write auth.json, or spawn Codex as a refresh fallback.
        let auth = File::open(auth_path)
            .ok()
            .and_then(|file| read_bounded(file, MAX_AUTH_BYTES))
            .and_then(|text| parse_auth(&text));
        let result = match auth {
            Some(auth) => fetch_usage(AGENT.get_or_init(new_agent), USAGE_URL, &auth, now),
            None => Err(PollError::Auth),
        };
        state.record(result, Instant::now());
    }
    state.fresh_sample(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_weekly_primary_and_secondary_without_relabeling_five_hour_usage() {
        let primary = r#"{"rate_limit":{"primary_window":{"limit_window_seconds":604800,"used_percent":7,"reset_at":9999},"secondary_window":null}}"#;
        assert_eq!(parse_usage(primary, 1000).unwrap().used, 7.0);
        let secondary = r#"{"rate_limit":{"primary_window":{"limit_window_seconds":18000,"used_percent":95,"reset_at":9999},"secondary_window":{"limit_window_seconds":604800,"used_percent":12,"reset_at":9999}}}"#;
        assert_eq!(parse_usage(secondary, 1000).unwrap().used, 12.0);
    }

    #[test]
    fn rejects_missing_invalid_expired_or_nonweekly_data() {
        for input in [
            "{}",
            r#"{"rate_limit":null}"#,
            r#"{"rate_limit":{"primary_window":{"limit_window_seconds":18000,"used_percent":7,"reset_at":9999}}}"#,
            r#"{"rate_limit":{"primary_window":{"limit_window_seconds":604800,"used_percent":101,"reset_at":9999}}}"#,
            r#"{"rate_limit":{"primary_window":{"limit_window_seconds":604800,"used_percent":7,"reset_at":999}}}"#,
        ] {
            assert!(parse_usage(input, 1000).is_none());
        }
    }

    #[test]
    fn validates_auth_without_using_api_keys_or_refresh_tokens() {
        assert!(
            parse_auth(r#"{"tokens":{"access_token":"test-token","account_id":"test-account"}}"#)
                .is_some()
        );
        for input in [
            "{}",
            r#"{"OPENAI_API_KEY":"test-api-key"}"#,
            r#"{"tokens":{"access_token":"","account_id":"a"}}"#,
            r#"{"tokens":{"access_token":"t","account_id":"a\r\nx: y"}}"#,
        ] {
            assert!(parse_auth(input).is_none());
        }
    }

    #[test]
    fn bounded_reads_reject_oversized_input() {
        assert_eq!(read_bounded("test".as_bytes(), 4).as_deref(), Some("test"));
        assert!(read_bounded("test!".as_bytes(), 4).is_none());
    }

    #[test]
    fn polling_caches_success_and_backs_off_without_refreshing_stale_timestamps() {
        let start = Instant::now();
        let mut state = PollState::default();
        assert!(state.due(start));
        state.record(
            Ok(Sample {
                timestamp: 1000,
                used: 7.0,
                reset_at: 9999,
            }),
            start,
        );
        assert!(!state.due(start + Duration::from_secs(59)));
        assert!(state.due(start + POLL_INTERVAL));
        state.record(Err(PollError::Http(429)), start + POLL_INTERVAL);
        assert!(!state.due(start + Duration::from_secs(359)));
        assert_eq!(state.fresh_sample(1100).unwrap().timestamp, 1000);
        assert!(state.fresh_sample(2201).is_none());
        state.record(
            Ok(Sample {
                timestamp: 2300,
                used: 8.0,
                reset_at: 9999,
            }),
            start,
        );
        assert_eq!(state.failures, 0);
        assert_eq!(state.fresh_sample(2300).unwrap().used, 8.0);
    }

    #[test]
    fn http_fixture_checks_headers_payload_and_refuses_redirects() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        for status in [200, 302, 401, 429] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/usage", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    request.push_str(&line);
                }
                let request = request.to_lowercase();
                assert!(request.starts_with("get /usage "));
                assert!(request.contains("authorization: bearer test-token"));
                assert!(request.contains("chatgpt-account-id: test-account"));
                let body = r#"{"rate_limit":{"primary_window":{"limit_window_seconds":604800,"used_percent":7,"reset_at":9999}}}"#;
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nLocation: http://127.0.0.1:1/leak\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let auth = Auth {
                access_token: "test-token".into(),
                account_id: "test-account".into(),
            };
            let result = fetch_usage(&new_agent(), &url, &auth, 1000);
            if status == 200 {
                assert_eq!(result.unwrap().used, 7.0);
            } else {
                assert!(matches!(result, Err(PollError::Http(code)) if code == status));
            }
            server.join().unwrap();
        }
    }
}
