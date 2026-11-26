// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS a `TooManyTagsException`, and what it leaves out of it.
//!
//! The rule under test, one sentence: **every call Amazon Bedrock refuses on the
//! per-resource tag quota — a merge into a resource's existing tags and a create's own
//! over-long `tags` list alike — carries ONE fixed sentence, which states the bound and
//! nothing of the request, so the optional `TooManyTagsException$resourceName` is left
//! UNSET.**
//!
//! That sentence states the bound (`MAX_TAGS_PER_RESOURCE`, the "50 tags per resource"
//! `TooManyTagsException` documents) and nothing else: no ARN, no tag key, no submitted
//! count, and nothing about WHICH set the quota was reached through.
//! `TooManyTagsException$resourceName` is modelled optional
//! (`smithy_query(query="shape",shape="TooManyTagsException")`: members `message` and
//! `resourceName`, neither `@required`), and this service leaves it out.
//!
//! The rows differ along what the rule generalises over:
//!
//! * which SET the quota is evaluated against — a merge into an existing resource's tags
//!   (`TagResource`, `POST /tagResource`) versus a create's own `tags` list
//!   (`CreateAutomatedReasoningPolicy`, `CreateAutomatedReasoningPolicyVersion`);
//! * HOW the bound is passed: the request alone past it with nothing held, the held set
//!   filling most of it, the bound already reached, and far past it;
//! * whether the refused batch also re-values keys the resource already carries, which do
//!   not grow the resultant set;
//! * the resource FAMILY the merge addresses: a guardrail, the draft of an Automated
//!   Reasoning policy and one version cut from it.
//!
//! `crate::tag_quota_across_resource_families` asserts the quota ARITHMETIC over the
//! families; this suite is about the refusal's wire contract. Both drive the shared harness
//! and the shared setup helpers (`crate::tagging`,
//! `crate::automated_reasoning_policy_tagging`) rather than a second copy of them.

use serde_json::{json, Value};

use crate::automated_reasoning_policy_tagging::{definition, given_policy, given_version};
use crate::support::{emulator, label, Emulator};
use crate::tagging::{
    given_guardrail, list_tags, listed, numbered_tags, owned_tag_list, tag, MAX_TAGS,
};

/// The one sentence Amazon Bedrock refuses a tag-quota call with, whichever set the quota
/// was reached through. Spelled here rather than read off the handler, so a re-wording of it
/// is caught: the bound is the one `TooManyTagsException` documents ("50 tags per
/// resource").
fn quota_sentence() -> String {
    format!("You can assign a maximum of {MAX_TAGS} tags to a resource")
}

/// The resource family a merging row addresses, each an alternative `TaggableResourcesArn`
/// spells.
#[derive(Clone, Copy, Debug)]
enum Family {
    Guardrail,
    PolicyDraft,
    PolicyVersion,
}

impl Family {
    /// Create one resource of this family, carrying no tags, and hand back the ARN a
    /// tagging call addresses it by.
    async fn given(self, emu: &Emulator, index: usize) -> String {
        match self {
            Family::Guardrail => given_guardrail(emu, &format!("tag-wording-guardrail-{index}")).await,
            Family::PolicyDraft => given_policy_draft(emu, index).await,
            Family::PolicyVersion => {
                let policy_arn = given_policy_draft(emu, index).await;
                given_version(emu, &policy_arn, &[]).await
            }
        }
    }
}

/// Create an Automated Reasoning policy carrying no tags and hand back the unversioned ARN
/// that names its draft.
async fn given_policy_draft(emu: &Emulator, index: usize) -> String {
    let (policy_arn, _) = given_policy(
        emu,
        json!({
            "name": format!("tag wording policy {index}"),
            "policyDefinition": definition(1),
        }),
    )
    .await;
    policy_arn
}

/// One way of passing the bound with a merging call.
struct MergeRow {
    case: &'static str,
    family: Family,
    /// How many tags the resource holds before the refused batch.
    held: usize,
    /// How many keys the resource does NOT carry the refused batch brings.
    fresh: usize,
    /// Whether the refused batch also re-values the first key the resource already carries.
    revalues_a_held_key: bool,
}

/// Every row's resultant set — the held keys together with the batch's fresh ones — is past
/// the bound, each by a different margin and from a different starting point.
const MERGE_ROWS: &[MergeRow] = &[
    MergeRow {
        case: "nothing held, the request alone past the bound",
        family: Family::Guardrail,
        held: 0,
        fresh: MAX_TAGS + 1,
        revalues_a_held_key: false,
    },
    MergeRow {
        case: "one tag held, the request fills the rest and one more",
        family: Family::PolicyDraft,
        held: 1,
        fresh: MAX_TAGS,
        revalues_a_held_key: false,
    },
    MergeRow {
        case: "the bound already reached, one fresh key",
        family: Family::PolicyVersion,
        held: MAX_TAGS,
        fresh: 1,
        revalues_a_held_key: false,
    },
    MergeRow {
        case: "far past the bound",
        family: Family::Guardrail,
        held: 10,
        fresh: MAX_TAGS * 2,
        revalues_a_held_key: false,
    },
    MergeRow {
        case: "fresh keys beside a re-valued held key",
        family: Family::PolicyDraft,
        held: 5,
        fresh: MAX_TAGS - 4,
        revalues_a_held_key: true,
    },
];

#[tokio::test]
async fn a_resultant_set_past_the_bound_is_refused_over_the_resultant_set() {
    let emu = emulator();
    // The sentence Amazon Bedrock words this refusal with, stating the bound.
    let expected_message = quota_sentence();

    for (index, row) in MERGE_ROWS.iter().enumerate() {
        let case = format!("{} / {:?}", row.case, row.family);
        let arn = row.family.given(&emu, index).await;

        let held = numbered_tags(0, row.held);
        if row.held > 0 {
            let applied = tag(&emu, &arn, owned_tag_list(&held)).await;
            assert_eq!(
                applied.status, 200,
                "case {case:?}: a within-quota batch of {} tags must answer 200, got {} {:?}",
                row.held, applied.status, applied.body
            );
        }
        let before = listed(&case, &list_tags(&emu, &arn).await);

        let mut batch = numbered_tags(row.held, row.fresh);
        if row.revalues_a_held_key {
            let (key, _) = held.first().unwrap_or_else(|| {
                panic!("case {case:?}: a row re-valuing a held key must hold one")
            });
            batch.push((key.clone(), "a new value".to_string()));
        }
        assert!(
            row.held + row.fresh > MAX_TAGS,
            "case {case:?}: the row must be past the bound of {MAX_TAGS}, got a resultant set \
             of {}",
            row.held + row.fresh
        );

        let refused = tag(&emu, &arn, owned_tag_list(&batch)).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: a resultant set of {} is past the bound of {MAX_TAGS}, so the call \
             must be refused 400, got {} {:?}",
            row.held + row.fresh,
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("TooManyTagsException"),
            "case {case:?}: the refusal is the modelled TooManyTagsException, got {:?}",
            refused.error
        );
        assert_eq!(
            refused.get("message").as_str(),
            Some(expected_message.as_str()),
            "case {case:?}: expected the message {expected_message:?}, got {:?}",
            refused.body
        );
        // Nothing of the request is quoted back: the optional member stays unset whatever
        // ARN, family or batch the row carried.
        assert_eq!(
            refused.get("resourceName"),
            Value::Null,
            "case {case:?}: resourceName must be unset, got {:?}",
            refused.body
        );

        // The refusal was decided before anything was applied, so the resource still
        // carries exactly the set the call found.
        let after = listed(&case, &list_tags(&emu, &arn).await);
        assert_eq!(
            after, before,
            "case {case:?}: a refused call must leave the tags it found, got {after:?}"
        );
    }
}

#[tokio::test]
async fn a_resultant_set_at_the_bound_is_not_refused() {
    let emu = emulator();
    // The bound itself: the sentence above is answered only PAST it, so a call landing
    // exactly on it is served and the resource carries the whole set.
    for (index, family) in [Family::Guardrail, Family::PolicyDraft, Family::PolicyVersion]
        .into_iter()
        .enumerate()
    {
        let case = format!("exactly at the bound / {family:?}");
        let arn = family.given(&emu, index).await;
        let whole = numbered_tags(0, MAX_TAGS);
        let applied = tag(&emu, &arn, owned_tag_list(&whole)).await;
        assert_eq!(
            applied.status, 200,
            "case {case:?}: a resultant set of exactly {MAX_TAGS} must answer 200, got {} {:?}",
            applied.status, applied.body
        );
        assert_eq!(
            listed(&case, &list_tags(&emu, &arn).await),
            whole,
            "case {case:?}: the whole batch must be associated"
        );
    }
}

/// One create whose own `tags` list is past the bound.
struct CreateRow {
    case: &'static str,
    /// How many tags the create's own list carries.
    tags: usize,
    /// Whether the create cuts a VERSION of a policy (so the request addresses a resource
    /// that already exists) rather than minting the policy itself.
    cuts_a_version: bool,
}

const CREATE_ROWS: &[CreateRow] = &[
    CreateRow {
        case: "a policy create one tag past the bound",
        tags: MAX_TAGS + 1,
        cuts_a_version: false,
    },
    CreateRow {
        case: "a policy create far past the bound",
        tags: MAX_TAGS * 3,
        cuts_a_version: false,
    },
    CreateRow {
        case: "a version create one tag past the bound",
        tags: MAX_TAGS + 1,
        cuts_a_version: true,
    },
    CreateRow {
        case: "a version create far past the bound",
        tags: MAX_TAGS + 17,
        cuts_a_version: true,
    },
];

#[tokio::test]
async fn a_creates_own_tag_list_past_the_bound_is_refused_over_that_list() {
    let emu = emulator();
    // A create is evaluated against the list it CARRIES, and the quota refusal it reads is
    // the service's one sentence for the quota: a client reads the same wording whichever
    // set the quota was reached through.
    let expected_message = quota_sentence();

    for (index, row) in CREATE_ROWS.iter().enumerate() {
        let case = row.case;
        let tags = owned_tag_list(&numbered_tags(0, row.tags));
        let refused = if row.cuts_a_version {
            let policy_arn = given_policy_draft(&emu, 100 + index).await;
            let read = emu
                .call(
                    "GET",
                    &format!("/automated-reasoning-policies/{}", label(&policy_arn)),
                    None,
                )
                .await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: GetAutomatedReasoningPolicy must answer 200, got {} {:?}",
                read.status, read.body
            );
            emu.call(
                "POST",
                &format!(
                    "/automated-reasoning-policies/{}/versions",
                    label(&policy_arn)
                ),
                Some(json!({
                    "lastUpdatedDefinitionHash": read.text("definitionHash"),
                    "tags": tags,
                })),
            )
            .await
        } else {
            emu.call(
                "POST",
                "/automated-reasoning-policies",
                Some(json!({
                    "name": format!("tag wording create {index}"),
                    "policyDefinition": definition(1),
                    "tags": tags,
                })),
            )
            .await
        };

        assert_eq!(
            refused.status, 400,
            "case {case:?}: {} tags is past the bound of {MAX_TAGS}, so the create must be \
             refused 400, got {} {:?}",
            row.tags, refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("TooManyTagsException"),
            "case {case:?}: the refusal is the modelled TooManyTagsException, got {:?}",
            refused.error
        );
        assert_eq!(
            refused.get("message").as_str(),
            Some(expected_message.as_str()),
            "case {case:?}: expected the message {expected_message:?}, got {:?}",
            refused.body
        );
        assert_eq!(
            refused.get("resourceName"),
            Value::Null,
            "case {case:?}: resourceName must be unset, got {:?}",
            refused.body
        );
    }
}
