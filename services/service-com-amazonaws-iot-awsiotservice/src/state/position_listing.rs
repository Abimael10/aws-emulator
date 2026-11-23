// SPDX-License-Identifier: CC0-1.0

//! Where a resource sits in a listing ordered by a POSITION — a creation date, a creation
//! rank — in the direction that listing runs.
//!
//! Several of this service's listings order their rows by WHEN the thing they report came
//! to be and carry an `ascendingOrder` flag for the direction
//! (`ListCertificates$ascendingOrder` is "Specifies the order for results. If True, the
//! results are returned in ascending order, based on the creation date",
//! `ListPrincipalPolicies$ascendingOrder` "If true, results are returned in ascending
//! creation order"). Paging such a listing needs ONE total order per direction, because a
//! page marker names a POSITION in the listing's order rather than a record
//! ([`super::page_of`], which walks rows ascending and resumes after the row a marker
//! names).
//!
//! So a row is the position rendered as a fixed-width decimal, followed by the name of
//! what sits there: the digits are what rows are compared by, and the name is what the
//! handler projects the entry from. A descending listing is the same position counted down
//! from the end of the range, which reverses the order while keeping every row comparable
//! with `<` — which is what lets ONE pager serve both directions.
//!
//! The one encoding, used by every position-ordered listing of this service, so two
//! orderings cannot drift: [`super::certificate_listing_row`] for the certificate
//! listings, [`super::attachment_listing_row`] for the policy-attachment one.

/// The character that separates a row's POSITION from the name it names. A control
/// character, so it sorts before every digit a position is rendered with and occurs in no
/// name a listing reports (a `certificateId` is modelled `^(0x)?[a-fA-F0-9]+$` and a
/// `policyName` `^[\w+=,.@-]+$`).
const ROW_SEPARATOR: char = '\u{1}';

/// How wide a row's position is rendered: enough decimal digits for every `u64`, so
/// positions compare as strings exactly as they compare as numbers.
const POSITION_WIDTH: usize = 20;

/// The row `name` occupies at `position` in a listing running in that direction, as a
/// string that sorts ASCENDING in the listing's own order whichever direction it runs.
pub fn position_listing_row(position: u64, ascending_order: bool, name: &str) -> String {
    let ordered = if ascending_order {
        position
    } else {
        u64::MAX.saturating_sub(position)
    };
    format!(
        "{ordered:0width$}{ROW_SEPARATOR}{name}",
        width = POSITION_WIDTH
    )
}

/// The name a listing row names, or `None` for a string that is not one
/// [`position_listing_row`] built.
pub fn position_listing_row_name(row: &str) -> Option<&str> {
    row.split_once(ROW_SEPARATOR).map(|(_position, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The positions every case below is checked over: both ends of the range, the
    /// neighbours of each end and a value in between, so a case covers the rendering as
    /// well as the comparison.
    const POSITIONS: [u64; 6] = [0, 1, 2, 1_700_000_000_000_000, u64::MAX - 1, u64::MAX];

    /// RULE: a row's order is its position's order in the listing's direction, and the row
    /// names exactly the name it was built from.
    #[test]
    fn rows_order_positions_in_the_listings_direction_and_name_them_back() {
        for ascending_order in [true, false] {
            for left in POSITIONS {
                let left_row = position_listing_row(left, ascending_order, "left-name");
                assert_eq!(
                    position_listing_row_name(&left_row),
                    Some("left-name"),
                    "case {left} ascending={ascending_order}: expected the row to name it, \
                     got {left_row:?}"
                );
                for right in POSITIONS {
                    let right_row = position_listing_row(right, ascending_order, "left-name");
                    let expected = if ascending_order {
                        left.cmp(&right)
                    } else {
                        right.cmp(&left)
                    };
                    assert_eq!(
                        left_row.cmp(&right_row),
                        expected,
                        "case {left} vs {right} ascending={ascending_order}: expected \
                         {expected:?}, got {left_row:?} against {right_row:?}"
                    );
                }
            }
        }
    }

    /// RULE: two rows of the same position are ordered by the name they name, the same way
    /// in both directions — which is what makes a listing's order total even where two
    /// things came to be at the same instant.
    #[test]
    fn one_position_is_ordered_by_the_name_it_names() {
        for ascending_order in [true, false] {
            for position in POSITIONS {
                let first = position_listing_row(position, ascending_order, "aaa");
                let second = position_listing_row(position, ascending_order, "aab");
                assert!(
                    first < second,
                    "case {position} ascending={ascending_order}: expected {first:?} before \
                     {second:?}"
                );
            }
        }
    }
}
