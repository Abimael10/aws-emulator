// SPDX-License-Identifier: CC0-1.0

//! Where a NAMED resource sits in a listing ordered by its name, in the direction the
//! listing runs.
//!
//! Several of this service's listings order their rows by the resource's NAME and carry
//! an `ascendingOrder` flag for the direction (`ListAuthorizers$ascendingOrder`: "Return
//! the list of authorizers in ascending alphabetical order",
//! `smithy_query(query="operation",operation="ListAuthorizers")`). Paging such a listing
//! needs ONE total order per direction, because a page marker names a POSITION in the
//! listing's order rather than a record ([`super::page_of`], which walks rows ascending
//! and resumes after the row a marker names).
//!
//! So a row is the name's COLLATION KEY for that direction, followed by the name itself:
//! the key is what rows are compared by, and the name is what the handler projects the
//! entry from. The ascending key is the name's bytes, hex-encoded — hex keeps the order
//! of the bytes it encodes, so the key order IS the name order — and the descending key
//! is the complement of those bytes, which reverses it, closed by
//! [`DESCENDING_TERMINATOR`] so a name sorts before the names it is a prefix of in that
//! direction too.

/// The character that separates a listing row's COLLATION KEY from the name it names.
///
/// A control character, so it sorts before every character a key can hold (hex digits
/// and [`DESCENDING_TERMINATOR`]) and a name that is another name's prefix keeps its
/// place in the ascending direction.
const ROW_SEPARATOR: char = '\u{1}';

/// What closes a DESCENDING collation key: a character that sorts AFTER every hex digit,
/// so a longer key — the name with more bytes — compares less than the key of the name it
/// extends, which is what the descending direction means for two names where one is the
/// other's prefix.
const DESCENDING_TERMINATOR: char = 'z';

/// `name`'s collation key in a listing running in the direction `ascending_order` names:
/// keys compare exactly as the names they encode do in that direction.
fn collation_key(name: &str, ascending_order: bool) -> String {
    if ascending_order {
        ::hex::encode(name)
    } else {
        let complemented: Vec<u8> = name.bytes().map(|byte| !byte).collect();
        format!("{}{DESCENDING_TERMINATOR}", ::hex::encode(complemented))
    }
}

/// The row `name` occupies in a name-ordered listing of that direction, as a string that
/// sorts ASCENDING in the listing's own order whichever direction it runs — which is what
/// lets one pager ([`super::page_of`]) serve both.
pub fn name_listing_row(name: &str, ascending_order: bool) -> String {
    format!(
        "{key}{ROW_SEPARATOR}{name}",
        key = collation_key(name, ascending_order)
    )
}

/// The rows `names` occupy in a listing of that direction, in the listing's order — the
/// total order [`name_listing_row`] defines, which is what makes a page boundary
/// meaningful.
pub fn name_listing_rows<'a>(
    names: impl Iterator<Item = &'a str>,
    ascending_order: bool,
) -> Vec<String> {
    let mut rows: Vec<String> = names
        .map(|name| name_listing_row(name, ascending_order))
        .collect();
    rows.sort();
    rows
}

/// The name a listing row names, or `None` for a string that is not one
/// [`name_listing_row`] built.
pub fn name_listing_row_name(row: &str) -> Option<&str> {
    row.split_once(ROW_SEPARATOR).map(|(_key, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names every case below is checked over: they span what a name-ordered listing
    /// generalises over — one name a prefix of another, the characters an
    /// `AuthorizerName` admits (`^[\w=,@-]+$`, so digits, `-`, `,`, `=`, `@` and `_`,
    /// each of which sorts either side of the row separator), case, and length.
    const NAMES: [&str; 10] = [
        "a",
        "ab",
        "ab-x",
        "ab,x",
        "ab=x",
        "ab@x",
        "ab_x",
        "Ab",
        "0",
        "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
    ];

    /// RULE: a row's order is the name's order in the listing's direction, and the row
    /// names exactly the name it was built from.
    #[test]
    fn rows_order_names_in_the_listings_direction_and_name_them_back() {
        for ascending_order in [true, false] {
            for left in NAMES {
                let left_row = name_listing_row(left, ascending_order);
                assert_eq!(
                    name_listing_row_name(&left_row),
                    Some(left),
                    "case {left:?} ascending={ascending_order}: expected the row to name it, \
                     got {left_row:?}"
                );
                for right in NAMES {
                    let right_row = name_listing_row(right, ascending_order);
                    let expected = if ascending_order {
                        left.cmp(right)
                    } else {
                        right.cmp(left)
                    };
                    assert_eq!(
                        left_row.cmp(&right_row),
                        expected,
                        "case {left:?} vs {right:?} ascending={ascending_order}: expected \
                         {expected:?}, got {left_row:?} against {right_row:?}"
                    );
                }
            }
        }
    }

    /// RULE: the rows of a listing are its order — ascending by name in one direction,
    /// descending in the other — however the names arrive.
    #[test]
    fn a_listings_rows_are_its_order() {
        for ascending_order in [true, false] {
            let rows = name_listing_rows(NAMES.iter().copied(), ascending_order);
            let listed: Vec<&str> = rows
                .iter()
                .map(|row| {
                    name_listing_row_name(row).expect("every row of a listing names its name")
                })
                .collect();
            let mut expected: Vec<&str> = NAMES.to_vec();
            expected.sort();
            if !ascending_order {
                expected.reverse();
            }
            assert_eq!(
                listed, expected,
                "case ascending={ascending_order}: expected {expected:?}, got {listed:?}"
            );
        }
    }
}
