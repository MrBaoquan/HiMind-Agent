use std::collections::HashSet;
use std::env;

#[derive(Debug, Clone)]
pub(crate) struct LocalRequestSecurity {
    allowed_origins: HashSet<String>,
    allowed_hosts: HashSet<String>,
}

impl LocalRequestSecurity {
    pub(crate) fn new(dashboard_base: &str, local_port: u16) -> Self {
        let mut allowed_origins = HashSet::new();
        if let Some(origin) = normalized_origin(dashboard_base) {
            allowed_origins.insert(origin);
        }
        if let Ok(configured) = env::var("HIMIND_AGENT_ALLOWED_ORIGINS") {
            for value in configured.split(',') {
                if let Some(origin) = normalized_origin(value.trim()) {
                    allowed_origins.insert(origin);
                }
            }
        }
        let allowed_hosts = [
            format!("127.0.0.1:{local_port}"),
            format!("localhost:{local_port}"),
            format!("[::1]:{local_port}"),
        ]
        .into_iter()
        .collect();
        Self {
            allowed_origins,
            allowed_hosts,
        }
    }

    pub(crate) fn validate<'a>(&self, request: &'a str) -> Result<Option<&'a str>, &'static str> {
        let host = header_value(request, "host").ok_or("missing Host header")?;
        if !self.allowed_hosts.contains(&host.to_ascii_lowercase()) {
            return Err("untrusted Host header");
        }
        let Some(origin) = header_value(request, "origin") else {
            return Ok(None);
        };
        if origin == "null" || !self.allowed_origins.contains(origin) {
            return Err("untrusted Origin header");
        }
        Ok(Some(origin))
    }
}

pub(crate) fn header_value<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

fn normalized_origin(value: &str) -> Option<String> {
    let value = value.trim().trim_end_matches('/');
    let (scheme, authority) = value.split_once("://")?;
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") || authority.is_empty() {
        return None;
    }
    let authority = authority.split('/').next()?.to_ascii_lowercase();
    Some(format!("{}://{}", scheme.to_ascii_lowercase(), authority))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn security() -> LocalRequestSecurity {
        LocalRequestSecurity::new("http://localhost:18081", 18181)
    }

    #[test]
    fn accepts_configured_dashboard_origin() {
        let request = "GET /health HTTP/1.1\r\nHost: 127.0.0.1:18181\r\nOrigin: http://localhost:18081\r\n\r\n";
        assert_eq!(
            security().validate(request),
            Ok(Some("http://localhost:18081"))
        );
    }

    #[test]
    fn accepts_local_script_without_origin() {
        let request = "GET /health HTTP/1.1\r\nHost: localhost:18181\r\n\r\n";
        assert_eq!(security().validate(request), Ok(None));
    }

    #[test]
    fn rejects_untrusted_browser_origin() {
        let request = "POST /remote-connect HTTP/1.1\r\nHost: 127.0.0.1:18181\r\nOrigin: https://evil.example\r\n\r\n";
        assert_eq!(security().validate(request), Err("untrusted Origin header"));
    }

    #[test]
    fn rejects_dns_rebinding_host() {
        let request = "GET /health HTTP/1.1\r\nHost: agent.evil.example\r\nOrigin: http://localhost:18081\r\n\r\n";
        assert_eq!(security().validate(request), Err("untrusted Host header"));
    }
}
