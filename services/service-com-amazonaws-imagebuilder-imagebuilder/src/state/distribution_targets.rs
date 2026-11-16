// SPDX-License-Identifier: CC0-1.0

//! Which target Region a distribution configuration may name, and how a target this service
//! cannot distribute through is worded.
//!
//! A `Distribution` entry is "the settings for a specific Region"
//! (`smithy_query(query="shape",shape="Distribution")`) and its `region` member — "The
//! target Region" — is a bare `NonEmptyString`, so the model cannot say that the value has
//! to NAME a Region at all. Image Builder resolves it to the endpoint it distributes the
//! output AMI through, so a value outside the Region grammar names no target: there is
//! nowhere to distribute to, and the entry is refused rather than stored.
//!
//! The grammar is the shared one (`service_common::arn::region_label`): the field every AWS
//! model's ARN pattern admits — non-empty, at most twenty characters, composed only of
//! lower-case letters, digits and hyphens.

/// The target Region of `region` when it names one at all, and `None` otherwise.
///
/// A target is named by a Region LABEL; anything else — a Region code in another case, a
/// value carrying a separator an endpoint authority would re-point on, a label longer than
/// the field admits, a word that is not a Region code — names no Region of this partition.
#[must_use]
pub fn distribution_target(region: &str) -> Option<&str> {
    ::service_common::arn::region_label(region)
}

/// The requirement clause this service states for a target Region it cannot distribute
/// through: that AMI distribution is not available there, quoting the target as the request
/// spelled it.
///
/// The caller has to see WHICH of its targets was refused, and the target is the only part
/// of the request the clause carries.
#[must_use]
pub fn unavailable_distribution_target_requirement(region: &str) -> String {
    format!("AMI Distribution is not available in '{region}'.")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a target is named by a Region label, and every other form names none.
    #[test]
    fn only_a_region_label_names_a_target() {
        // (case, the value the entry carried, whether it names a target)
        let rows = [
            ("a Region code", "us-west-2", true),
            ("a Region code of three hyphenated words", "ap-southeast-3", true),
            ("a label of the longest length the field admits", "us-west-2-us-west-20", true),
            ("a Region code in upper case", "US-EAST-2", false),
            ("a Region code in mixed case", "Eu-West-1", false),
            ("a value carrying a path separator", "us-west-2/../elsewhere", false),
            ("a value carrying a port separator", "us-west-2:443", false),
            ("a label one character past the length the field admits", "us-west-2-us-west-201", false),
            ("an empty value", "", false),
            ("a word that is not a Region code", "Oregon", false),
        ];

        for (case, value, expected) in rows {
            let actual = distribution_target(value).is_some();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected names-a-target {expected}, got {actual}"
            );
        }
    }

    /// RULE: the clause states that AMI distribution is unavailable there and quotes the
    /// target as the request spelled it.
    #[test]
    fn the_clause_quotes_the_target_it_refuses() {
        for region in ["US-EAST-2", "Eu-West-1", "Oregon"] {
            let actual = unavailable_distribution_target_requirement(region);
            let expected = format!("AMI Distribution is not available in '{region}'.");
            assert_eq!(
                actual, expected,
                "case {region:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
