// SPDX-License-Identifier: CC0-1.0

//! The condition keys the SESSION TAGS one mint ATTACHES publish about that mint, for every
//! service that mints a tagged session.
//!
//! A mint that attaches session tags is decided on those tags by the role's own trust
//! policy, and AWS publishes them to that decision under two global keys (AWS General
//! Reference, *Global condition context keys*): `aws:RequestTag/<key>`, which holds the
//! value the mint asks to attach under one tag key, and the multi-valued `aws:TagKeys`,
//! which carries every tag key it attaches. They answer for the tags of THIS mint rather
//! than for the ones the caller already carries (`aws:PrincipalTag/<key>`).
//!
//! Two services mint such a session — STS, from the `Tags` an `AssumeRole` passes and the
//! principal tags a presented web identity token carries, and Amazon Cognito Identity, from
//! the principal tags its own exchange maps — so the rendering lives here rather than once
//! per crate: a trust policy written over one of those keys must be decided alike whichever
//! mint it gates.

/// The condition-key FAMILY the tags a mint attaches are named in, each member after the tag
/// key it carries: `aws:RequestTag/Department` holds the value the mint attaches under the
/// tag key `Department`.
pub const REQUEST_TAG_CONDITION_KEY_FAMILY: &str = "aws:RequestTag/";

/// The condition key carrying every tag KEY the mint attaches, which is what a trust policy
/// that bounds WHICH tags may be passed is written over. Multi-valued, so a policy decides it
/// under one of IAM's set qualifiers.
pub const TAG_KEYS_CONDITION_KEY: &str = "aws:TagKeys";

/// The keys a mint that can attach a session tag PUBLISHES, beside the ones the exchange
/// itself publishes: declared so a clause over a tag the mint does NOT attach is decided as
/// absent rather than left undecided. A mint knows every tag it attaches, so both are always
/// answerable.
pub const ATTACHED_TAG_PUBLISHED_CONDITION_KEYS: &[&str] =
    &[REQUEST_TAG_CONDITION_KEY_FAMILY, TAG_KEYS_CONDITION_KEY];

/// The condition keys `tags` publish about the mint attaching them: one
/// `aws:RequestTag/<key>` per tag, and one [`TAG_KEYS_CONDITION_KEY`] value per tag key
/// (a multi-valued key is passed as one pair per value, which is how
/// [`crate::iam_authorization`] reads one).
///
/// Appended to the keys the exchange itself publishes, so ONE context decides the trust
/// policy: who is assuming, what the exchange established, and the tags the mint asks to
/// attach.
#[must_use]
pub fn attached_tag_conditions(tags: &[(String, String)]) -> Vec<(String, String)> {
    let mut context = Vec::with_capacity(tags.len() * 2);
    for (key, value) in tags {
        context.push((
            format!("{REQUEST_TAG_CONDITION_KEY_FAMILY}{key}"),
            value.clone(),
        ));
        context.push((TAG_KEYS_CONDITION_KEY.to_string(), key.clone()));
    }
    context
}

#[cfg(test)]
mod tests {
    use super::{attached_tag_conditions, ATTACHED_TAG_PUBLISHED_CONDITION_KEYS};

    /// One row of the table below: the case, the tags attached, and the context they publish.
    type ConditionRow = (
        &'static str,
        &'static [(&'static str, &'static str)],
        &'static [(&'static str, &'static str)],
    );

    /// One tag publishes its own key and one entry of the tag-key list; no tag publishes
    /// nothing, and the pairs keep the order the tags were given in.
    #[test]
    fn a_tag_publishes_its_own_key_and_the_tag_key_list() {
        // (case, the tags attached, the context they publish)
        let rows: &[ConditionRow] = &[
            ("no tag at all", &[], &[]),
            (
                "one tag",
                &[("Department", "eng")],
                &[
                    ("aws:RequestTag/Department", "eng"),
                    ("aws:TagKeys", "Department"),
                ],
            ),
            (
                "several tags",
                &[("Department", "eng"), ("Tenant", "acme")],
                &[
                    ("aws:RequestTag/Department", "eng"),
                    ("aws:TagKeys", "Department"),
                    ("aws:RequestTag/Tenant", "acme"),
                    ("aws:TagKeys", "Tenant"),
                ],
            ),
            (
                "a tag carrying an empty value",
                &[("Tenant", "")],
                &[("aws:RequestTag/Tenant", ""), ("aws:TagKeys", "Tenant")],
            ),
        ];
        for (case, tags, expected) in rows {
            let attached: Vec<(String, String)> = tags
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect();
            let actual = attached_tag_conditions(&attached);
            let expected: Vec<(String, String)> = expected
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
        assert_eq!(
            ATTACHED_TAG_PUBLISHED_CONDITION_KEYS,
            ["aws:RequestTag/", "aws:TagKeys"],
            "case \"the published keys\": expected the two families a tagged mint answers for, \
             got {ATTACHED_TAG_PUBLISHED_CONDITION_KEYS:?}"
        );
    }
}
