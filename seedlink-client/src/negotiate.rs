/// Parse capabilities from the `extra` field of a HELLO response.
///
/// The extra field may look like:
/// - `"(2020.075) :: SLPROTO:4.0 SLPROTO:3.1"` — contains `"::"` separator
/// - `"SLPROTO:4.0 SLPROTO:3.1"` — already stripped by `parse_hello` when no extra text
///
/// We split on `"::"` and parse tokens from the right side. If no `"::"` is found,
/// we look for capability-style tokens (containing `:`) in the full string.
pub fn parse_capabilities(extra: &str) -> Vec<String> {
    if let Some(idx) = extra.find("::") {
        let right = extra[idx + 2..].trim();
        if right.is_empty() {
            return Vec::new();
        }
        return right.split_whitespace().map(|s| s.to_owned()).collect();
    }

    // No "::" separator — check if the string itself contains capability tokens
    let tokens: Vec<String> = extra
        .split_whitespace()
        .filter(|t| t.contains(':'))
        .map(|s| s.to_owned())
        .collect();
    tokens
}

/// Check if capabilities include SeedLink v4 support.
pub fn supports_v4(capabilities: &[String]) -> bool {
    capabilities.iter().any(|c| c == "SLPROTO:4.0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_with_v4() {
        let caps = parse_capabilities("(2020.075) :: SLPROTO:4.0 SLPROTO:3.1");
        assert_eq!(caps, vec!["SLPROTO:4.0", "SLPROTO:3.1"]);
        assert!(supports_v4(&caps));
    }

    #[test]
    fn parse_without_v4() {
        let caps = parse_capabilities("(2020.075) :: SLPROTO:3.1");
        assert_eq!(caps, vec!["SLPROTO:3.1"]);
        assert!(!supports_v4(&caps));
    }

    #[test]
    fn parse_empty_extra() {
        let caps = parse_capabilities("");
        assert!(caps.is_empty());
        assert!(!supports_v4(&caps));
    }

    #[test]
    fn parse_no_separator_no_caps() {
        let caps = parse_capabilities("(2020.075)");
        assert!(caps.is_empty());
    }

    #[test]
    fn parse_no_separator_with_caps() {
        // parse_hello may strip "::" leaving just capability tokens
        let caps = parse_capabilities("SLPROTO:4.0 SLPROTO:3.1");
        assert_eq!(caps, vec!["SLPROTO:4.0", "SLPROTO:3.1"]);
        assert!(supports_v4(&caps));
    }

    #[test]
    fn parse_separator_but_empty_right() {
        let caps = parse_capabilities("(2020.075) ::  ");
        assert!(caps.is_empty());
    }

    #[test]
    fn parse_multiple_capabilities() {
        let caps = parse_capabilities(":: SLPROTO:4.0 CAP:AUTH CAP:WINDOW");
        assert_eq!(caps, vec!["SLPROTO:4.0", "CAP:AUTH", "CAP:WINDOW"]);
        assert!(supports_v4(&caps));
    }

    #[test]
    fn supports_v4_empty() {
        assert!(!supports_v4(&[]));
    }
}
