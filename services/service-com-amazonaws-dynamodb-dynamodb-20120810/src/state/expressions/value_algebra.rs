// SPDX-License-Identifier: CC0-1.0

//! What ONE attribute VALUE is to the expression languages: how two of them compare, how
//! long one is, the arithmetic an update computes with them, and the set algebra `ADD` and
//! `DELETE` apply to them.
//!
//! A file of its own beside the expression readers rather than inside them: these answer
//! questions about a value's own domain — its order, its size, its members — and none of
//! them reads an expression or an item. [`super`] re-exports them, so every caller still
//! names them `crate::state::…`.

use super::*;

/// The refusal for a collection past [`MAX_COLLECTION_MEMBERS`], whether a request sent
/// it or an update's own `ADD`/`list_append` built it.
pub(super) fn too_many_members(name: &str, members: usize) -> RequestError {
    RequestError::Validation(format!(
        "Attribute {name} holds {members} members, more than this emulator's maximum of \
         {MAX_COLLECTION_MEMBERS}"
    ))
}

/// A set member's canonical sort key, so that members compare and deduplicate by the
/// VALUE they denote: two `N` spellings of one number share one key.
pub(super) fn canonical_number(text: &str) -> String {
    NumberKey::parse(text).map_or_else(|| text.to_string(), |number| number.canonical_text())
}

/// Whether two sets hold the same members. A DynamoDB set is unordered, so this is
/// member-wise rather than position-wise.
///
/// Both sides are SORTED by `key` rather than walked pairwise: the member count comes
/// from the request, so a pairwise walk would cost the product of two caller-chosen
/// counts, where sorting costs `n log n`.
pub(super) fn same_members<'v, T: 'v, K: Ord>(
    left: &'v [T],
    right: &'v [T],
    key: impl Fn(&'v T) -> K,
) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let key = &key;
    let mut mine: Vec<K> = left.iter().map(key).collect();
    let mut theirs: Vec<K> = right.iter().map(key).collect();
    mine.sort_unstable();
    theirs.sort_unstable();
    mine == theirs
}

/// How two attribute values order, or `None` when DynamoDB documents no order between
/// them: "For type Number, value comparisons are numeric. String value comparisons for
/// greater than, equals, or less than are based on ASCII character code values. For
/// Binary, DynamoDB treats each byte of the binary data as unsigned when it compares
/// binary values" (the `AttributeValueList` doc) — and nothing for the other types, or
/// across types.
pub(crate) fn compare_values(left: &AttributeValue, right: &AttributeValue) -> Option<Ordering> {
    match (left, right) {
        (AttributeValue::S(mine), AttributeValue::S(theirs)) => Some(mine.cmp(theirs)),
        (AttributeValue::B(mine), AttributeValue::B(theirs)) => Some(mine.cmp(theirs)),
        (AttributeValue::N(mine), AttributeValue::N(theirs)) => {
            Some(NumberKey::parse(mine)?.cmp(&NumberKey::parse(theirs)?))
        }
        _ => None,
    }
}

/// `begins_with (path, operand)` — "true if the attribute specified by path begins
/// with a particular substring", which only a string or binary value can.
pub(super) fn begins_with_value(value: &AttributeValue, prefix: &AttributeValue) -> bool {
    match (value, prefix) {
        (AttributeValue::S(text), AttributeValue::S(prefix)) => text.starts_with(prefix.as_str()),
        (AttributeValue::B(bytes), AttributeValue::B(prefix)) => bytes.starts_with(prefix),
        _ => false,
    }
}

/// `contains (path, operand)` — "true if the attribute specified by path is: A String
/// that contains a particular substring, [or] A Set that contains a particular
/// element". A list is searched for the element too, the way `CONTAINS` reads it.
///
/// A binary value is a sequence of BYTES, and "DynamoDB treats each byte of the binary
/// data as unsigned when it compares binary values" (`AttributeValueList`), so a `B`
/// haystack contains a `B` needle when the needle's bytes appear in it as a contiguous
/// run — any byte value included, `0x00` among them.
pub(super) fn contains_value(haystack: &AttributeValue, needle: &AttributeValue) -> bool {
    match (haystack, needle) {
        (AttributeValue::S(text), AttributeValue::S(part)) => text.contains(part.as_str()),
        (AttributeValue::B(bytes), AttributeValue::B(part)) => contains_bytes(bytes, part),
        (AttributeValue::Ss(members), AttributeValue::S(member)) => members.contains(member),
        (AttributeValue::Ns(members), AttributeValue::N(member)) => {
            members.iter().any(|stored| numbers_equal(stored, member))
        }
        (AttributeValue::Bs(members), AttributeValue::B(member)) => members.contains(member),
        (AttributeValue::L(members), needle) => members
            .iter()
            .any(|stored| attribute_values_equal(stored, needle)),
        _ => false,
    }
}

/// Whether `part` appears in `bytes` as a contiguous run of bytes. The empty run is in
/// every sequence, the way the empty string is a substring of every string.
///
/// Both lengths are the CALLER's — the haystack is a stored `B` value and the needle an
/// operand the request carried — so the search is the linear-time one `str::contains`
/// uses for text (`memchr::memmem`), and what one evaluation costs is the SUM of the two
/// lengths rather than their product.
pub(super) fn contains_bytes(bytes: &[u8], part: &[u8]) -> bool {
    if part.is_empty() {
        return true;
    }
    if part.len() > bytes.len() {
        return false;
    }
    ::memchr::memmem::find(bytes, part).is_some()
}

/// `size (path)` — the length of the value at a path: UTF-16 CODE UNITS for a string,
/// bytes for a binary value, members for a set, elements for a list, entries for a map.
/// Nothing else has a documented size.
///
/// A string's length is counted in the code units its characters are carried in rather
/// than in the bytes its UTF-8 transfer encoding happens to need, so `size` answers the
/// same number for a string whatever script it is written in: a three-character string
/// of the basic multilingual plane is three, and a character outside that plane is the
/// two code units a surrogate pair spells it with. Counting the transfer bytes instead
/// would make every predicate over a non-ASCII string select a different set.
pub(super) fn size_of_value(value: &AttributeValue) -> Option<i64> {
    Some(match value {
        AttributeValue::S(text) => byte_len(text.encode_utf16().count()),
        AttributeValue::B(bytes) => byte_len(bytes.len()),
        AttributeValue::Ss(members) => byte_len(members.len()),
        AttributeValue::Ns(members) => byte_len(members.len()),
        AttributeValue::Bs(members) => byte_len(members.len()),
        AttributeValue::L(values) => byte_len(values.len()),
        AttributeValue::M(entries) => byte_len(entries.len()),
        AttributeValue::N(_) | AttributeValue::Bool(_) | AttributeValue::Null(_) => return None,
    })
}

/// Add or subtract two `N` values, reporting the canonical text of the result.
///
/// The arithmetic is exact decimal arithmetic (the `bigdecimal` crate), so
/// `SET n = n + :v` and `ADD n :v` answer what DynamoDB answers rather than what binary
/// floating point would. The result is read back as a stored number
/// ([`NumberKey::try_parse`]), the same reading a written number goes through, so a
/// computed number is refused by the rule it breaks and stored in the one form every
/// read reports.
///
/// The result is NOT rounded to fit: a number carries at most the documented 38
/// significant digits inside the documented exponent range, and a sum that needs more
/// of either is a number this store cannot hold — so the update is refused rather than
/// silently writing a value the operands do not add up to.
///
/// A result the domain cannot hold is a COMPUTATION that left it, so it is reported as
/// the numeric OVERFLOW it is whichever bound it crossed — the significant digits or the
/// magnitude. The precision clause is a statement about a number an input member
/// CARRIED, and no member carried this one: the expression asked for it, which is also
/// why the clause is stated on its own, out of the validation envelope
/// ([`unenveloped`]).
pub fn number_arithmetic(
    left: &str,
    right: &str,
    subtract: bool,
) -> Result<String, RequestError> {
    let left_value = parse_decimal(left)?;
    let right_value = parse_decimal(right)?;
    let result = if subtract {
        left_value - right_value
    } else {
        left_value + right_value
    };
    let (digits, scale) = result.as_bigint_and_exponent();
    // `digits * 10^-scale` is the value, which is exactly what an `N` text spells.
    let text = format!("{digits}e{}", -scale);
    NumberKey::try_parse(&text)
        .map(|number| number.canonical_text())
        .map_err(|fault| unenveloped(number_rejection(&text, computed_number_fault(fault))))
}

/// The rule a COMPUTED number breaks, as that number's refusal states it: a result
/// carrying more significant digits than the domain holds is reported as the overflow
/// the computation ran into rather than as the precision clause a stored value earns.
///
/// Every other fault is the one the reader named: a magnitude outside the documented
/// exponent range already is that overflow (or the underflow at the other end), and the
/// operands were read as numbers before the arithmetic, so nothing here is unparsable.
pub(super) fn computed_number_fault(fault: NumberFault) -> NumberFault {
    match fault {
        NumberFault::Precision => NumberFault::Overflow,
        other => other,
    }
}

/// One operand of that arithmetic. The canonical text is what reaches the decimal
/// parser, so only one spelling of a number ever does, and a text that denotes no
/// number this store can hold is refused by the rule it breaks.
pub(super) fn parse_decimal(text: &str) -> Result<BigDecimal, RequestError> {
    let canonical = canonical_number_text(text)?;
    BigDecimal::from_str(&canonical)
        .map_err(|_| number_rejection(text, NumberFault::NotNumeric))
}

/// The members of a set value, as the member-wise set operations below walk them.
pub(super) enum SetMembers<'a> {
    Strings(&'a [String]),
    Numbers(&'a [String]),
    Binaries(&'a [Vec<u8>]),
}

/// A value's set members, or `None` when it is not a set at all.
pub(super) fn set_members(value: &AttributeValue) -> Option<SetMembers<'_>> {
    match value {
        AttributeValue::Ss(members) => Some(SetMembers::Strings(members)),
        AttributeValue::Ns(members) => Some(SetMembers::Numbers(members)),
        AttributeValue::Bs(members) => Some(SetMembers::Binaries(members)),
        _ => None,
    }
}

/// How many members a set holds, for the empty-set refusal.
pub(super) fn set_len(value: &AttributeValue) -> Option<usize> {
    Some(match set_members(value)? {
        SetMembers::Strings(members) | SetMembers::Numbers(members) => members.len(),
        SetMembers::Binaries(members) => members.len(),
    })
}

/// The members a union would hold at most, refused past [`MAX_COLLECTION_MEMBERS`]
/// BEFORE the union is built: a repeated `ADD` must not grow one stored set past the
/// ceiling every value is admitted under.
pub(super) fn check_union_members(stored: usize, incoming: usize) -> Result<(), RequestError> {
    let total = stored.saturating_add(incoming);
    if total > MAX_COLLECTION_MEMBERS {
        return Err(too_many_members("the set being added to", total));
    }
    Ok(())
}

/// `ADD`'s set form: "If the existing data type is a set and if Value is also a set,
/// then Value is added to the existing set". Both sets must hold the same primitive
/// type, and a member the set already holds is not added twice.
///
/// Membership is tested against a SORTED index of the stored members rather than by
/// scanning them per incoming member: both counts come from the request, so a scan per
/// member would cost their product.
pub(super) fn set_union(
    existing: &AttributeValue,
    addition: &AttributeValue,
) -> Result<AttributeValue, RequestError> {
    match (existing, addition) {
        (AttributeValue::Ss(stored), AttributeValue::Ss(incoming)) => {
            check_union_members(stored.len(), incoming.len())?;
            let mut held: BTreeSet<&str> = stored.iter().map(String::as_str).collect();
            let mut members = stored.clone();
            for member in incoming {
                if held.insert(member.as_str()) {
                    members.push(member.clone());
                }
            }
            Ok(AttributeValue::Ss(members))
        }
        (AttributeValue::Ns(stored), AttributeValue::Ns(incoming)) => {
            check_union_members(stored.len(), incoming.len())?;
            let mut held: BTreeSet<String> = stored.iter().map(|text| canonical_number(text)).collect();
            let mut members = stored.clone();
            for member in incoming {
                if held.insert(canonical_number(member)) {
                    members.push(member.clone());
                }
            }
            Ok(AttributeValue::Ns(members))
        }
        (AttributeValue::Bs(stored), AttributeValue::Bs(incoming)) => {
            check_union_members(stored.len(), incoming.len())?;
            let mut held: BTreeSet<&[u8]> = stored.iter().map(Vec::as_slice).collect();
            let mut members = stored.clone();
            for member in incoming {
                if held.insert(member.as_slice()) {
                    members.push(member.clone());
                }
            }
            Ok(AttributeValue::Bs(members))
        }
        (_held, _incoming) => Err(incorrect_update_operand_type()),
    }
}

/// `DELETE`'s set form: "those values are subtracted from the old set". An empty
/// result is no set at all, so the attribute goes away with its last member. The
/// members to remove are indexed first, for the reason [`set_union`] gives.
pub(super) fn set_difference(
    existing: &AttributeValue,
    removal: &AttributeValue,
) -> Result<Option<AttributeValue>, RequestError> {
    let remaining = match (existing, removal) {
        (AttributeValue::Ss(stored), AttributeValue::Ss(removed)) => {
            let gone: BTreeSet<&str> = removed.iter().map(String::as_str).collect();
            let members: Vec<String> = stored
                .iter()
                .filter(|member| !gone.contains(member.as_str()))
                .cloned()
                .collect();
            (!members.is_empty()).then_some(AttributeValue::Ss(members))
        }
        (AttributeValue::Ns(stored), AttributeValue::Ns(removed)) => {
            let gone: BTreeSet<String> = removed.iter().map(|text| canonical_number(text)).collect();
            let members: Vec<String> = stored
                .iter()
                .filter(|member| !gone.contains(&canonical_number(member)))
                .cloned()
                .collect();
            (!members.is_empty()).then_some(AttributeValue::Ns(members))
        }
        (AttributeValue::Bs(stored), AttributeValue::Bs(removed)) => {
            let gone: BTreeSet<&[u8]> = removed.iter().map(Vec::as_slice).collect();
            let members: Vec<Vec<u8>> = stored
                .iter()
                .filter(|member| !gone.contains(member.as_slice()))
                .cloned()
                .collect();
            (!members.is_empty()).then_some(AttributeValue::Bs(members))
        }
        (_held, _removed) => return Err(incorrect_update_operand_type()),
    };
    Ok(remaining)
}
