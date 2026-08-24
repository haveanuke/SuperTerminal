//! QR matrix for the phone-link flyout: wraps the encoder into a plain
//! row-major (modules, size) pair so rendering stays a dumb grid of quads.

/// Boolean module matrix (row-major, `size * size` entries, true = dark)
/// for `url`. None if encoding fails (input too long for any QR version —
/// unreachable for companion links, but the flyout then simply omits the
/// code instead of panicking).
pub fn matrix(url: &str) -> Option<(Vec<bool>, usize)> {
    // ECL M: plenty of redundancy for a bright on-screen scan without the
    // module density a higher level would force on a small flyout card.
    let qr = fast_qr::QRBuilder::new(url)
        .ecl(fast_qr::ECL::M)
        .build()
        .ok()?;
    let size = qr.size;
    let modules = qr.data[..size * size]
        .iter()
        .map(|module| module.value())
        .collect();
    Some((modules, size))
}

#[cfg(test)]
mod tests {
    use super::matrix;

    const URL: &str = "http://100.111.22.33:43110#0123456789abcdef0123456789abcdef";

    #[test]
    fn companion_url_encodes_to_square_matrix() {
        let (modules, size) = matrix(URL).expect("companion URLs always fit");
        assert_eq!(modules.len(), size * size);
        // Valid QR side lengths are 21 + 4k.
        assert!(size >= 21 && (size - 21) % 4 == 0, "size {size}");
    }

    #[test]
    fn finder_pattern_anchors_top_left() {
        // Orientation/indexing proof: the top-left 7x7 finder has a dark
        // border ring, a white inner band, and a dark 3x3 center.
        let (m, s) = matrix(URL).unwrap();
        assert!(m[0], "(0,0) border dark");
        assert!(m[6 * s + 6], "(6,6) border dark");
        assert!(!m[s + 1], "(1,1) band light");
        assert!(m[3 * s + 3], "(3,3) center dark");
    }

    #[test]
    fn deterministic_for_same_url() {
        assert_eq!(matrix(URL), matrix(URL));
    }
}
