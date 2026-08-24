//! Capability token: the page-level authorization on top of Tailscale.
//! 128 bits from /dev/urandom, carried in the bookmark's URL fragment
//! (fragments never appear in HTTP requests or referrers). Comparison is
//! constant-time so timing can't leak prefix matches.

pub fn generate_token() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .expect("/dev/urandom is always readable on macOS");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time equality: accumulate XOR over the longer length (zero
/// padded) and fold in the length difference, so neither content nor length
/// short-circuits.
pub fn token_matches(expected: &str, presented: &str) -> bool {
    let expected = expected.as_bytes();
    let presented = presented.as_bytes();
    let len = expected.len().max(presented.len());
    let mut diff = (expected.len() ^ presented.len()) as u8;
    for i in 0..len {
        let a = expected.get(i).copied().unwrap_or(0);
        let b = presented.get(i).copied().unwrap_or(0);
        diff |= a ^ b;
    }
    diff == 0 && !expected.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_32_lowercase_hex_chars() {
        let token = generate_token();
        assert_eq!(token.len(), 32);
        assert!(token
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn two_generations_differ() {
        assert_ne!(generate_token(), generate_token());
    }

    #[test]
    fn matches_only_exact() {
        assert!(token_matches("abcd1234", "abcd1234"));
        assert!(!token_matches("abcd1234", "abcd1235"));
        assert!(!token_matches("abcd1234", "abcd123"));
        assert!(!token_matches("abcd1234", "abcd12345"));
        assert!(!token_matches("abcd1234", ""));
        assert!(!token_matches("", "abcd1234"));
    }
}
