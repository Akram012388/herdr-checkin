//! Pure decoding for Herdr's public identifiers. IDs remain opaque command targets everywhere else;
//! this module decodes only the stable public pane allocation number for friendly display fallback.

/// Herdr 0.7.5's bijective base-32 public-ID alphabet. It omits ambiguous letters and uses `0` as
/// digit 32; generated pane suffixes therefore run `1`..`9`, `A`..`Z` (with gaps), `0`, `11`, ...
const PUBLIC_ID_ALPHABET: &[u8; 32] = b"123456789ABCDEFGHJKMNPQRSTVWXYZ0";

/// Decode the stable public pane allocation number in a complete Herdr pane ID (`wT:pA` -> `10`).
/// This is not visual pane order and may have gaps because closed allocations are not reused.
/// Unknown syntax and arithmetic overflow return `None` so callers can render a neutral fallback.
pub(crate) fn pane_public_number(pane_id: &str) -> Option<usize> {
    let (workspace, segment) = pane_id.rsplit_once(':')?;
    if workspace.is_empty() {
        return None;
    }
    let encoded = segment.strip_prefix('p')?;
    if encoded.is_empty() {
        return None;
    }

    encoded.bytes().try_fold(0usize, |value, byte| {
        let digit = PUBLIC_ID_ALPHABET
            .iter()
            .position(|candidate| *candidate == byte)?
            + 1;
        value
            .checked_mul(PUBLIC_ID_ALPHABET.len())?
            .checked_add(digit)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_herdrs_bijective_base_32_pane_numbers() {
        for (pane_id, number) in [
            ("w1:p1", 1),
            ("wT:p9", 9),
            ("wT:pA", 10),
            ("wT:pT", 26),
            ("wT:p0", 32),
            ("wT:p11", 33),
            ("wT:p10", 64),
        ] {
            assert_eq!(pane_public_number(pane_id), Some(number), "{pane_id}");
        }
    }

    #[test]
    fn rejects_non_pane_ids_unknown_digits_and_overflow() {
        for pane_id in ["wT:p", "wT:pI", "wT:qA", "pA", ":pA"] {
            assert_eq!(pane_public_number(pane_id), None, "{pane_id}");
        }
        let overflow = format!("wT:p{}", "0".repeat(usize::BITS as usize));
        assert_eq!(pane_public_number(&overflow), None);
    }
}
