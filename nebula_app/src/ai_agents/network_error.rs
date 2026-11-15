use super::contains_unquoted;

/// Explicit transport failures; a bare HTTP 502 or interrupted stream is inconclusive.
pub(crate) fn reason(error: &str) -> Option<&'static str> {
    let error = error.to_ascii_lowercase();
    [
        "no such host",
        "temporary failure in name resolution",
        "could not resolve host",
        "could not resolve proxy",
        "getaddrinfo enotfound",
        "getaddrinfo eai_again",
        "getaddrinfo eai_noname",
        "network is unreachable",
        "network is down",
        "no route to host",
        "err_internet_disconnected",
        "connection refused",
    ]
    .into_iter()
    .find(|reason| contains_unquoted(&error, reason))
}

#[cfg(test)]
mod tests {
    use super::super::{auto_continue_error_text, network_error_reason};

    #[test]
    fn network_error_detection_ignores_retry_patterns_but_rejects_quoted_history() {
        for program in ["claude", "codex"] {
            let marker = if program == "claude" { "● API Error:" } else { "■" };
            for reason in [
                "dial tcp: lookup chatgpt.com: no such host",
                "Temporary failure in name resolution",
                "Could not resolve host: chatgpt.com",
                "Could not resolve proxy: localhost",
                "getaddrinfo ENOTFOUND chatgpt.com",
                "getaddrinfo EAI_AGAIN chatgpt.com",
                "getaddrinfo EAI_NONAME chatgpt.com",
                "connect: network is unreachable",
                "connect: network is down",
                "connect: no route to host",
                "net::ERR_INTERNET_DISCONNECTED",
                "connect: connection refused",
            ] {
                let error =
                    format!("{marker} 502 {reason}. This is a server-side issue; try again.");
                assert!(auto_continue_error_text(program, &error, &[]).is_some(), "{error}");
            }
            for body in [
                "The guide says no such host",
                "API Error: copied text `no such host`",
                "```\nAPI Error: no such host\n```",
            ] {
                assert!(auto_continue_error_text(program, body, &[]).is_none(), "{body}");
            }
        }
    }

    #[test]
    fn network_error_cause_survives_long_lines_and_generic_502_stays_retryable() {
        let long_error = format!("API Error: 502 {}: no such host", "x".repeat(600));
        let detected = auto_continue_error_text("claude", &long_error, &[]).unwrap();
        assert_eq!(network_error_reason(&detected), Some("no such host"));
        assert!(detected.chars().count() <= 512);
        for error in [
            "API Error: 502 server error, try again in a moment",
            "■ stream disconnected before completion: response.failed event received",
            "API Error: copied text `no such host`",
        ] {
            assert!(network_error_reason(error).is_none(), "{error}");
        }
    }
}
