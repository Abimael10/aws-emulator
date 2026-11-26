// SPDX-License-Identifier: CC0-1.0

//! The CLASS and the WORDING of three refusals of this control plane, and the one listing
//! that is not a refusal at all, asserted on the wire.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder, the stage deploy
//! helper and the ARN builder are this crate's existing harness, reused from
//! `rest_api_lifecycle`, `stage_lifecycle`, `tag_reads_by_arn` and `usage_plan_lifecycle`.
//!
//! The RULES under test, each over a SET of inputs rather than one recorded call:
//!
//! 1. A `stageName` a request SUPPLIES that names no stage of the addressed api is a bad
//!    INPUT to the call (400 `BadRequestException`), not a report that the resource being
//!    created was not found (404) — the class a client branches on — and it is stated with
//!    this service's own sentence for a stage identifier that names no record.
//! 2. The per-resource TAG QUOTA is refused with one fixed sentence that names the published
//!    limit and the all-or-nothing effect, and interpolates neither the resulting count nor
//!    any key — whichever path charges the quota, and however the request reaches past it.
//! 3. "Which API keys does this usage plan meter?" is answered with the keys it meters, so a
//!    `usagePlanId` that names no plan of the account is served the EMPTY page rather than
//!    refused: a listing of nothing is not a missing resource.

use serde_json::{json, Map, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    tag_quota_reached, MAX_TAGS, NO_SUCH_STAGE,
};

use crate::documentation_versions::{create_version, get_version};
use crate::rest_api_lifecycle::{create_api, page_items, string_member, Answer, Gateway};
use crate::stage_lifecycle::create_deployed_stage;
use crate::tag_reads_by_arn::{apigateway_arn, encode_label, get_tags};
use crate::usage_plan_lifecycle::create_usage_plan;

const BAD_REQUEST: &str = "BadRequestException";

/// Assert `answer` is a refusal with `status`, `code` and exactly `message`.
fn assert_refusal(answer: &Answer, status: u16, code: &str, message: &str, case: &str) {
    assert_eq!(
        answer.status, status,
        "case {case}: expected HTTP {status}, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error.as_deref(),
        Some(code),
        "case {case}: expected {code}, got {:?} with {:?}",
        answer.error,
        answer.body
    );
    assert_eq!(
        answer.body["message"].as_str(),
        Some(message),
        "case {case}: expected the message {message:?}, got {:?}",
        answer.body["message"]
    );
}

#[tokio::test]
async fn a_documentation_version_for_a_stage_the_api_does_not_have_is_a_bad_request() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "documentation-version-stage").await;
    let stage = "prod";
    create_deployed_stage(&gateway, &api, stage).await;

    // A stage of ANOTHER api, and a stage of this api that is then DELETED: both are names
    // that really name a stage somewhere, which a name-shaped check would miss.
    let (other_api, _other_root) = create_api(&gateway, "documentation-version-other").await;
    create_deployed_stage(&gateway, &other_api, "elsewhere").await;
    create_deployed_stage(&gateway, &api, "doomed").await;
    let deleted = gateway
        .call("DELETE", &format!("/restapis/{api}/stages/doomed"), None)
        .await;
    assert_eq!(
        deleted.status, 202,
        "case DeleteStage: expected the modelled 202, got {} with {:?}",
        deleted.status, deleted.body
    );

    // Rows differ in the FORM the name takes that names no stage of THIS api: never
    // created, the stage of another api, a deleted stage, the stage's name in another case,
    // and the api's own id.
    let refused = [
        "nowhere",
        "elsewhere",
        "doomed",
        "PROD",
        api.as_str(),
    ];
    for (index, name) in refused.iter().enumerate() {
        let case = format!("CreateDocumentationVersion(stageName={name:?})");
        let version = format!("refused-{index}");
        let answer = create_version(
            &gateway,
            &api,
            json!({ "documentationVersion": version, "stageName": name }),
        )
        .await;
        assert_refusal(&answer, 400, BAD_REQUEST, NO_SUCH_STAGE, &case);
        // The refused request stored no snapshot.
        let read = get_version(&gateway, &api, &version).await;
        assert_eq!(
            read.status, 404,
            "case {case}: a refused create must store no snapshot, got {} with {:?}",
            read.status, read.body
        );
    }

    // The same call naming the api's real stage is served, so the refusals above are about
    // the name and not about the operation.
    let created = create_version(
        &gateway,
        &api,
        json!({ "documentationVersion": "1.0.0", "stageName": stage }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case CreateDocumentationVersion(the api's own stage): expected 201, got {} with {:?}",
        created.status, created.body
    );
    assert_eq!(
        string_member(&created.body, "version", "the api's own stage"),
        "1.0.0",
        "case CreateDocumentationVersion(the api's own stage): expected the identifier back, \
         got {:?}",
        created.body
    );
}

/// `TagResource` for `resource_arn` with `tags`.
async fn tag(gateway: &Gateway, resource_arn: &str, tags: Value) -> Answer {
    gateway
        .call(
            "PUT",
            &format!("/tags/{}", encode_label(resource_arn)),
            Some(json!({ "tags": tags })),
        )
        .await
}

/// `n` distinct tags, keyed `{prefix}{index}`.
fn tags(prefix: &str, n: usize) -> Value {
    Value::Object(
        (0..n)
            .map(|index| {
                (
                    format!("{prefix}{index}"),
                    Value::String(format!("v{index}")),
                )
            })
            .collect::<Map<String, Value>>(),
    )
}

#[tokio::test]
async fn a_tag_write_past_the_per_resource_quota_states_the_limit_and_sets_no_tag() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "tag-quota-sentence").await;
    let resource_arn = apigateway_arn(&format!("/restapis/{api}"));

    // The collection is filled to a few tags short of the limit, so a request can reach
    // past it by adding more keys than the slack that is left.
    let slack = 2;
    let filled = MAX_TAGS - slack;
    let accepted = tag(&gateway, &resource_arn, tags("k", filled)).await;
    assert_eq!(
        accepted.status, 204,
        "case TagResource({filled} tags): expected the modelled 204, got {} with {:?}",
        accepted.status, accepted.body
    );

    // Rows differ in HOW the request reaches past the quota: one key too many, several too
    // many, exactly the limit's worth of NEW keys in one request, and more than the limit in
    // one request — the last two reach past it on the request's own count rather than on
    // the merge.
    let rows: Vec<(String, Value)> = vec![
        (
            format!("{} new keys onto {filled}", slack + 1),
            tags("new", slack + 1),
        ),
        (
            format!("{} new keys onto {filled}", slack + 5),
            tags("new", slack + 5),
        ),
        (
            format!("{MAX_TAGS} new keys onto {filled}"),
            tags("new", MAX_TAGS),
        ),
        (
            format!("{} keys in one request", MAX_TAGS + 1),
            tags("over", MAX_TAGS + 1),
        ),
    ];
    let expected = tag_quota_reached();
    for (case, requested) in &rows {
        let answer = tag(&gateway, &resource_arn, requested.clone()).await;
        assert_refusal(&answer, 400, BAD_REQUEST, &expected, case);
        // "No tags were set on this request": the collection is exactly what it was.
        let read = get_tags(&gateway, &resource_arn).await;
        let stored = read.body["tags"].as_object().cloned().unwrap_or_default();
        assert_eq!(
            stored.len(),
            filled,
            "case {case}: a refused write must set no tag, got {stored:?}"
        );
    }

    // A create that carries more tags than the quota admits is refused with the same
    // sentence, so the wording belongs to the quota and not to one operation.
    let created = gateway
        .call(
            "POST",
            "/restapis",
            Some(json!({ "name": "tag-quota-create", "tags": tags("c", MAX_TAGS + 1) })),
        )
        .await;
    assert_refusal(
        &created,
        400,
        BAD_REQUEST,
        &expected,
        "CreateRestApi past the tag quota",
    );

    // And a write that lands exactly ON the limit is accepted: the refusals above are
    // about passing it.
    let onto_the_limit = tag(&gateway, &resource_arn, tags("new", slack)).await;
    assert_eq!(
        onto_the_limit.status, 204,
        "case TagResource(exactly onto the limit): expected the modelled 204, got {} with {:?}",
        onto_the_limit.status, onto_the_limit.body
    );
    let read = get_tags(&gateway, &resource_arn).await;
    let stored = read.body["tags"].as_object().cloned().unwrap_or_default();
    assert_eq!(
        stored.len(),
        MAX_TAGS,
        "case TagResource(exactly onto the limit): expected {MAX_TAGS} tags, got {stored:?}"
    );
}

#[tokio::test]
async fn the_keys_of_a_usage_plan_id_that_names_no_plan_are_the_empty_page() {
    let gateway = Gateway::new();
    // One stored plan, so the empty pages below are about the identifier and not about an
    // account that holds nothing.
    let created = create_usage_plan(&gateway, "key-listing", json!({ "name": "key-listing" })).await;
    let plan = string_member(&created, "id", "CreateUsagePlan");

    // Rows differ in the FORM the identifier takes and in the query the listing carries: a
    // plan that was never minted, an id-shaped value, the stored plan's id with a character
    // changed, and a value that is no id at all.
    let mut chars: Vec<char> = plan.chars().collect();
    chars[0] = if chars[0] == 'a' { 'b' } else { 'a' };
    let near_miss: String = chars.into_iter().collect();
    for identifier in ["sim2real-missing-usage-plan", "0000000000", near_miss.as_str()] {
        for query in ["", "limit=1", "limit=500", "name=anything"] {
            let case = format!("GetUsagePlanKeys({identifier:?}, {query:?})");
            let uri = if query.is_empty() {
                format!("/usageplans/{identifier}/keys")
            } else {
                format!("/usageplans/{identifier}/keys?{query}")
            };
            let answer = gateway.call("GET", &uri, None).await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected the modelled 200, got {} with {:?}",
                answer.status, answer.body
            );
            assert!(
                page_items(&answer.body, &case).is_empty(),
                "case {case}: expected an empty page, got {:?}",
                answer.body
            );
            assert!(
                answer.body["position"].is_null(),
                "case {case}: an empty page is the last one and omits position, got {:?}",
                answer.body["position"]
            );
        }
    }

    // The stored plan's own listing is served the same way — empty, because nothing meters
    // it yet — so the answer follows the associations and not the identifier's existence.
    let stored = gateway
        .call("GET", &format!("/usageplans/{plan}/keys"), None)
        .await;
    assert_eq!(
        stored.status, 200,
        "case GetUsagePlanKeys(the stored plan): expected 200, got {} with {:?}",
        stored.status, stored.body
    );
    assert!(
        page_items(&stored.body, "GetUsagePlanKeys(the stored plan)").is_empty(),
        "case GetUsagePlanKeys(the stored plan): expected an empty page, got {:?}",
        stored.body
    );
}
