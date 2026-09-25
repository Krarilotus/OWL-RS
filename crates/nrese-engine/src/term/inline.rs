//! Inline value encoding for literals whose value fits into a [`TermId`] payload.

use oxrdf::vocab::xsd;
use oxrdf::{Literal, LiteralRef};

use super::{PAYLOAD_BITS, TermId, TermKind};

const INT_MIN: i64 = -(1 << (PAYLOAD_BITS - 1));
const INT_MAX: i64 = (1 << (PAYLOAD_BITS - 1)) - 1;
const PAYLOAD_MASK: u64 = (1 << PAYLOAD_BITS) - 1;

/// Returns the inline id for `literal` if it is a canonical `xsd:integer` in the
/// 60-bit range or a canonical `xsd:boolean`. O(len(lexical)).
pub(crate) fn try_inline_literal(literal: LiteralRef<'_>) -> Option<TermId> {
    if literal.language().is_some() {
        return None;
    }
    let datatype = literal.datatype();
    let lexical = literal.value();
    if datatype == xsd::INTEGER {
        let value: i64 = lexical.parse().ok()?;
        if !(INT_MIN..=INT_MAX).contains(&value) || !is_canonical_integer(lexical, value) {
            return None;
        }
        Some(TermId::new(
            TermKind::Integer,
            (value as u64) & PAYLOAD_MASK,
        ))
    } else if datatype == xsd::BOOLEAN {
        match lexical {
            "true" => Some(TermId::new(TermKind::Boolean, 1)),
            "false" => Some(TermId::new(TermKind::Boolean, 0)),
            _ => None,
        }
    } else {
        None
    }
}

/// Canonical xsd:integer: no leading '+', no leading zeros, no "-0".
fn is_canonical_integer(lexical: &str, value: i64) -> bool {
    // i64::to_string is the canonical form; comparing lengths first avoids the allocation
    // in the common mismatch cases.
    let digits = lexical.strip_prefix('-').unwrap_or(lexical);
    if lexical.starts_with('+') || (digits.len() > 1 && digits.starts_with('0')) {
        return false;
    }
    !(value == 0 && lexical.starts_with('-'))
}

pub(crate) fn decode_integer(payload: u64) -> i64 {
    // Sign-extend the 60-bit two's complement payload.
    ((payload << (64 - PAYLOAD_BITS)) as i64) >> (64 - PAYLOAD_BITS)
}

/// Materialises an inline id back into a literal. Panics are impossible for ids produced
/// by [`try_inline_literal`]; other kinds return `None`.
pub(crate) fn inline_to_literal(id: TermId) -> Option<Literal> {
    match id.kind() {
        TermKind::Integer => Some(Literal::new_typed_literal(
            decode_integer(id.payload()).to_string(),
            xsd::INTEGER,
        )),
        TermKind::Boolean => Some(Literal::new_typed_literal(
            if id.payload() != 0 { "true" } else { "false" },
            xsd::BOOLEAN,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(lexical: &str) -> Option<TermId> {
        try_inline_literal(LiteralRef::new_typed_literal(lexical, xsd::INTEGER))
    }

    #[test]
    fn canonical_integers_are_inlined_and_roundtrip() {
        for lexical in [
            "0",
            "1",
            "-1",
            "42",
            "-123456789",
            &INT_MAX.to_string(),
            &INT_MIN.to_string(),
        ] {
            let id = int(lexical).unwrap_or_else(|| panic!("{lexical} should inline"));
            assert_eq!(inline_to_literal(id).unwrap().value(), lexical);
        }
    }

    #[test]
    fn non_canonical_or_out_of_range_integers_stay_in_dictionary() {
        for lexical in [
            "01",
            "+1",
            "-0",
            "00",
            " 1",
            &(INT_MAX as i128 + 1).to_string(),
            "abc",
        ] {
            assert!(int(lexical).is_none(), "{lexical} must not inline");
        }
    }

    #[test]
    fn booleans() {
        let t = try_inline_literal(LiteralRef::new_typed_literal("true", xsd::BOOLEAN)).unwrap();
        assert_eq!(t.as_inline_boolean(), Some(true));
        assert!(try_inline_literal(LiteralRef::new_typed_literal("1", xsd::BOOLEAN)).is_none());
    }
}
