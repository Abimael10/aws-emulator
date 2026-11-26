// SPDX-License-Identifier: CC0-1.0

//! WHICH sentence a `TooManyTagsException` carries, of the two Amazon Bedrock words the
//! per-resource tag quota with.
//!
//! The rule under test, one sentence: **a tagging call refused on the per-resource tag quota
//! reports the bound it broke — a request whose OWN tag list is longer than one resource may
//! hold is refused over that list, while a request inside that bound whose RESULTANT tag set
//! (what the resource already carries together with what the call brings) would be longer is
//! refused over that resultant set** — both of them HTTP 400 `TooManyTagsException`, with
//! `resourceName` unset and the bound (`MAX_TAGS`, the "50 tags per resource"
//! `TooManyTagsException` documents) as the only variable part of either sentence.
//!
//! The rows span what the rule generalises over: how the call breaks the quota (its own list
//! alone, the resultant set alone, and both at once), by what margin, how many tags the
//! resource already holds (none, a few, the bound itself), whether the refused batch also
//! re-values a key the resource already carries — which does not grow the resultant set — and
//! the resource FAMILY addressed (a guardrail and the draft of an Automated Reasoning policy,
//! two of the alternatives `TaggableResourcesArn` spells). The call that lands exactly ON the
//! bound is a row too, because a sentence is owed only PAST it.
//!
//! Each sentence is spelled out here rather than read off the handler, so a re-wording of
//! either fails these rows. `crate::tag_quota_refusal_wording` holds the rest of the wire
//! contract of the refusal and `crate::tag_quota_across_resource_families` the quota
//! arithmetic; all three drive the shared harness and the shared setup helpers rather than a
//! second copy of them.

use serde_json::{json, Value};

use crate::automated_reasoning_policy_tagging::{definition, given_policy};
use crate::support::{emulator, Emulator};
use crate::tagging::{
    given_guardrail, list_tags, listed, numbered_tags, owned_tag_list, tag, MAX_TAGS,
};

/// The sentence a call whose OWN tag list is past the bound is refused with.
fn submitted_list_sentence() -> String {
    format!("You can assign a maximum of {MAX_TAGS} tags to a resource")
}

/// The sentence a call inside that bound whose RESULTANT tag set would pass it is refused
/// with.
fn resultant_set_sentence() -> String {
    format!("The resultant tag set must not have more than {MAX_TAGS} user tags.")
}

/// Which of the two bounds a row breaks, and therefore which sentence it must read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Broke {
    /// The tag list the request itself carries is longer than one resource may hold.
    SubmittedList,
    /// The request's own list is inside the bound; the set it would LEAVE behind is not.
    ResultantSet,
    /// Neither: the call lands inside the quota and is served.
    Nothing,
}

/// The family a row's resource belongs to.
#[derive(Clone, Copy, Debug)]
enum Family {
    Guardrail,
    PolicyDraft,
}

impl Family {
    /// Create one resource of this family carrying no tags, and hand back the ARN a tagging
    /// call addresses it by.
    async fn given(self, emu: &Emulator, index: usize) -> String {
        match self {
            Family::Guardrail => {
                given_guardrail(emu, &format!("tag-bound-guardrail-{index}")).await
            }
            Family::PolicyDraft => {
                let (policy_arn, _) = given_policy(
                    emu,
                    json!({
                        "name": format!("tag bound policy {index}"),
                        "policyDefinition": definition(1),
                    }),
                )
                .await;
                policy_arn
            }
        }
    }
}

/// One tagging call against a resource holding `held` tags.
struct Row {
    case: &'static str,
    family: Family,
    /// How many tags the resource holds before the call.
    held: usize,
    /// How many keys the resource does NOT carry the call brings.
    fresh: usize,
    /// Whether the call also re-values the first key the resource already carries.
    revalues_a_held_key: bool,
    /// Which bound the call breaks.
    broke: Broke,
}

const ROWS: &[Row] = &[
    Row {
        case: "nothing held, the request's own list one past the bound",
        family: Family::Guardrail,
        held: 0,
        fresh: MAX_TAGS + 1,
        revalues_a_held_key: false,
        broke: Broke::SubmittedList,
    },
    Row {
        case: "nothing held, the request's own list far past the bound",
        family: Family::PolicyDraft,
        held: 0,
        fresh: MAX_TAGS * 4,
        revalues_a_held_key: false,
        broke: Broke::SubmittedList,
    },
    Row {
        case: "a few held, and a list past the bound on its own too",
        family: Family::Guardrail,
        held: 3,
        fresh: MAX_TAGS + 2,
        revalues_a_held_key: false,
        broke: Broke::SubmittedList,
    },
    Row {
        case: "the bound already reached, one fresh key",
        family: Family::Guardrail,
        held: MAX_TAGS,
        fresh: 1,
        revalues_a_held_key: false,
        broke: Broke::ResultantSet,
    },
    Row {
        case: "one tag held, a list of exactly the bound",
        family: Family::PolicyDraft,
        held: 1,
        fresh: MAX_TAGS,
        revalues_a_held_key: false,
        broke: Broke::ResultantSet,
    },
    Row {
        case: "half the bound held, a list that carries it past",
        family: Family::Guardrail,
        held: MAX_TAGS / 2,
        fresh: MAX_TAGS / 2 + 1,
        revalues_a_held_key: false,
        broke: Broke::ResultantSet,
    },
    Row {
        case: "fresh keys beside a re-valued held key, past the bound by one",
        family: Family::PolicyDraft,
        held: 5,
        fresh: MAX_TAGS - 4,
        revalues_a_held_key: true,
        broke: Broke::ResultantSet,
    },
    Row {
        case: "the bound reached, a batch that only re-values held keys",
        family: Family::Guardrail,
        held: MAX_TAGS,
        fresh: 0,
        revalues_a_held_key: true,
        broke: Broke::Nothing,
    },
    Row {
        case: "a list landing exactly on the bound",
        family: Family::PolicyDraft,
        held: 10,
        fresh: MAX_TAGS - 10,
        revalues_a_held_key: false,
        broke: Broke::Nothing,
    },
];

#[tokio::test]
async fn a_refused_tagging_call_reports_the_bound_it_broke() {
    let emu = emulator();
    for (index, row) in ROWS.iter().enumerate() {
        let case = format!("{} / {:?}", row.case, row.family);
        let arn = row.family.given(&emu, index).await;

        // The within-quota set the resource starts from.
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

        // The batch under test: keys the resource does not carry, and — where the row asks
        // for it — one key it does, which does not grow the resultant set.
        let mut batch = numbered_tags(row.held, row.fresh);
        if row.revalues_a_held_key {
            let (key, _) = held.first().unwrap_or_else(|| {
                panic!("case {case:?}: a row re-valuing a held key must hold one")
            });
            batch.push((key.clone(), "a new value".to_string()));
        }
        // The row's own arithmetic, so a row that does not reach the bound it claims to
        // break fails here rather than silently asserting the other sentence.
        let resultant = row.held + row.fresh;
        match row.broke {
            Broke::SubmittedList => assert!(
                batch.len() > MAX_TAGS,
                "case {case:?}: the row must submit more than {MAX_TAGS} tags, got {}",
                batch.len()
            ),
            Broke::ResultantSet => assert!(
                batch.len() <= MAX_TAGS && resultant > MAX_TAGS,
                "case {case:?}: the row must submit at most {MAX_TAGS} tags and leave more, \
                 got a batch of {} and a resultant set of {resultant}",
                batch.len()
            ),
            Broke::Nothing => assert!(
                batch.len() <= MAX_TAGS && resultant <= MAX_TAGS,
                "case {case:?}: the row must stay inside the quota, got a batch of {} and a \
                 resultant set of {resultant}",
                batch.len()
            ),
        }

        let reply = tag(&emu, &arn, owned_tag_list(&batch)).await;

        let Some(expected_message) = (match row.broke {
            Broke::SubmittedList => Some(submitted_list_sentence()),
            Broke::ResultantSet => Some(resultant_set_sentence()),
            Broke::Nothing => None,
        }) else {
            // Inside the quota: served, and the whole batch is associated.
            assert_eq!(
                reply.status, 200,
                "case {case:?}: a call inside the quota must answer 200, got {} {:?}",
                reply.status, reply.body
            );
            let after = listed(&case, &list_tags(&emu, &arn).await);
            assert_eq!(
                after.len(),
                resultant.max(row.held),
                "case {case:?}: the resource must carry its whole resultant set, got {after:?}"
            );
            continue;
        };

        assert_eq!(
            reply.status, 400,
            "case {case:?}: a call past the quota must be refused 400, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error.as_deref(),
            Some("TooManyTagsException"),
            "case {case:?}: the refusal is the modelled TooManyTagsException, got {:?}",
            reply.error
        );
        assert_eq!(
            reply.get("message").as_str(),
            Some(expected_message.as_str()),
            "case {case:?}: expected the message {expected_message:?}, got {:?}",
            reply.body
        );
        // Neither sentence quotes any part of the request back.
        assert_eq!(
            reply.get("resourceName"),
            Value::Null,
            "case {case:?}: resourceName must be unset, got {:?}",
            reply.body
        );
        // The refusal was decided before anything was applied.
        let after = listed(&case, &list_tags(&emu, &arn).await);
        assert_eq!(
            after, before,
            "case {case:?}: a refused call must leave the tags it found, got {after:?}"
        );
    }
}
