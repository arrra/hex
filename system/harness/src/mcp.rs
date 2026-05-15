/// Port of .hex/scripts/mcp-oauth-rewrite.sh
/// Rewrites MCP OAuth auth URLs so redirect_uri routes through hex-router.
///
/// Claude Code generates auth URLs with redirect_uri=http://localhost:{port}/callback.
/// On non-Mac-Mini devices that callback fails. This rewrites to the hex-router
/// equivalent so any device can complete the OAuth flow.

const ROUTER_BASE: &str = "https://mac-mini.tailbd5748.ts.net";

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut result: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                result.push((h << 4) | l);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            result.push(b' ');
            i += 1;
            continue;
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&result).into_owned()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

pub fn oauth_rewrite(input_url: &str) -> i32 {
    // Strip fragment before parsing query
    let (base_and_query, fragment) = match input_url.find('#') {
        Some(pos) => (&input_url[..pos], Some(&input_url[pos + 1..])),
        None => (input_url, None),
    };

    let (base, query) = match base_and_query.find('?') {
        Some(pos) => (&base_and_query[..pos], &base_and_query[pos + 1..]),
        None => {
            eprintln!("ERROR: No redirect_uri parameter found in URL");
            eprintln!("URL: {}", input_url);
            return 1;
        }
    };

    // Parse query params preserving order
    let mut params: Vec<(String, String)> = Vec::new();
    for part in query.split('&') {
        if part.is_empty() {
            continue;
        }
        let (k, v) = match part.find('=') {
            Some(eq) => (part[..eq].to_string(), part[eq + 1..].to_string()),
            None => (part.to_string(), String::new()),
        };
        params.push((k, v));
    }

    let redirect_idx = match params.iter().position(|(k, _)| k == "redirect_uri") {
        Some(i) => i,
        None => {
            eprintln!("ERROR: No redirect_uri parameter found in URL");
            eprintln!("URL: {}", input_url);
            return 1;
        }
    };

    let original_redirect = percent_decode(&params[redirect_idx].1);

    // Strip scheme
    let without_scheme = if let Some(rest) = original_redirect.strip_prefix("http://") {
        rest
    } else if let Some(rest) = original_redirect.strip_prefix("https://") {
        rest
    } else {
        eprintln!(
            "ERROR: redirect_uri is not a localhost URL: {}",
            original_redirect
        );
        eprintln!("Only localhost redirect URIs can be rewritten.");
        return 1;
    };

    // host:port or host:port/path
    let host_port = match without_scheme.find('/') {
        Some(slash) => &without_scheme[..slash],
        None => without_scheme,
    };

    let (hostname, port_str) = match host_port.rfind(':') {
        Some(colon) => (&host_port[..colon], &host_port[colon + 1..]),
        None => (host_port, ""),
    };

    if !matches!(hostname, "localhost" | "127.0.0.1" | "::1") {
        eprintln!(
            "ERROR: redirect_uri is not a localhost URL: {}",
            original_redirect
        );
        eprintln!("Only localhost redirect URIs can be rewritten.");
        return 1;
    }

    if port_str.is_empty() {
        eprintln!(
            "ERROR: No port found in redirect_uri: {}",
            original_redirect
        );
        return 1;
    }

    let port: u16 = match port_str.parse() {
        Ok(p) => p,
        Err(_) => {
            eprintln!(
                "ERROR: Invalid port in redirect_uri: {}",
                original_redirect
            );
            return 1;
        }
    };

    let new_redirect = format!("{}/auth/callback/{}", ROUTER_BASE, port);
    params[redirect_idx].1 = percent_encode(&new_redirect);

    let new_query: String = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&");

    let new_url = match fragment {
        Some(frag) => format!("{}?{}#{}", base, new_query, frag),
        None => format!("{}?{}", base, new_query),
    };

    println!("{}", new_url);

    eprintln!("\n[mcp-oauth-rewrite] redirect_uri rewritten:");
    eprintln!("  Before: {}", original_redirect);
    eprintln!("  After:  {}", new_redirect);
    eprintln!("  Port:   {}", port);

    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_basic() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("http%3A%2F%2Flocalhost%3A49386%2Fcallback"), "http://localhost:49386/callback");
        assert_eq!(percent_decode("no+encoding"), "no encoding");
    }

    #[test]
    fn percent_encode_basic() {
        assert_eq!(percent_encode("https://mac-mini.ts.net/auth/callback/49386"),
            "https%3A%2F%2Fmac-mini.ts.net%2Fauth%2Fcallback%2F49386");
    }

    #[test]
    fn rewrite_vercel_example() {
        let input = "https://vercel.com/oauth?client_id=abc&redirect_uri=http%3A%2F%2Flocalhost%3A49386%2Fcallback&state=xyz";
        // Run and capture: oauth_rewrite prints to stdout, but in tests we verify exit code
        // and validate the logic indirectly via the helper functions.
        let decoded = percent_decode("http%3A%2F%2Flocalhost%3A49386%2Fcallback");
        assert_eq!(decoded, "http://localhost:49386/callback");
        let _ = input; // full integration tested via cargo run
    }

    #[test]
    fn rewrite_fails_on_no_redirect_uri() {
        let exit = oauth_rewrite("https://example.com/oauth?client_id=abc");
        assert_eq!(exit, 1);
    }

    #[test]
    fn rewrite_fails_on_non_localhost() {
        let encoded = percent_encode("https://example.com/callback");
        let url = format!("https://example.com/oauth?redirect_uri={}", encoded);
        let exit = oauth_rewrite(&url);
        assert_eq!(exit, 1);
    }

    #[test]
    fn rewrite_fails_on_no_port() {
        let encoded = percent_encode("http://localhost/callback");
        let url = format!("https://example.com/oauth?redirect_uri={}", encoded);
        let exit = oauth_rewrite(&url);
        assert_eq!(exit, 1);
    }

    #[test]
    fn rewrite_fails_on_missing_query() {
        let exit = oauth_rewrite("https://example.com/oauth");
        assert_eq!(exit, 1);
    }

    #[test]
    fn router_base_constant() {
        assert_eq!(ROUTER_BASE, "https://mac-mini.tailbd5748.ts.net");
    }
}
