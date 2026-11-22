// SPDX-License-Identifier: CC0-1.0

//! What the tag READ says when its `ResourceArn` names no resource this endpoint holds.
//!
//! RULE: `GetTags` resolves its `ResourceArn` through the surface that OWNS the kind the
//! ARN's resource-type segment names, so the modelled `EntityNotFoundException` it answers
//! (HTTP 400) carries THAT surface's own miss sentence rather than one generic sentence for
//! every kind — "Job not found." for a job ARN, the database surface's sentence naming the
//! database for a database ARN, the crawler-definition surface's naming the crawler for a
//! crawler ARN, and the generic entity sentence for a kind the read is recorded to answer
//! that way (a workflow's, whose own surface words its miss exactly so). The name a sentence
//! carries comes from the ARN the request sent, resolved as the store keys that kind
//! ([`service_com_amazonaws_glue_awsglue::state::fold_name`] for the kinds the catalog
//! folds); the ARN itself is never echoed.
//!
//! Every way an ARN can name nothing is the same miss: a name nothing was created under, a
//! name that differs only in case from a stored one for a kind whose names are not folded, a
//! resource that was deleted, another account's estate, and another Region's — each of which
//! is THIS endpoint holding no such resource.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler. The creates and the
//! tag-set helpers are the tag-write suite's ([`crate::resource_tagging`]) and the per-kind
//! sentence is the one the kinds suite pins ([`crate::tagging_resource_kinds`]), so one
//! harness drives every kind.
//!
//! The rows differ along what the rule generalises over: which KIND the ARN names (one whose
//! surface words its own miss with a fixed sentence, one whose sentence names the resource,
//! and one that answers the generic sentence), WHY the ARN resolves to nothing, and which
//! Region's endpoint serves the call.

use serde_json::json;

use crate::resource_tagging::{create_request, glue_arn, map_of, read_tags, resource_arn};
use crate::tagging_resource_kinds::read_miss_message;
use crate::wire::{
    call, call_in_region, emulator, error_code, error_message, ok, ACCOUNT_ID, REGION,
};

/// The Region whose ARNs and whose endpoint stand in for "not the one serving the request".
/// The recorded call is served in one Region and names an ARN of another.
const OTHER_REGION: &str = "us-west-2";

/// An account other than the one this emulator serves, so an ARN of it names a resource this
/// endpoint holds nothing for whatever the name inside it is.
const OTHER_ACCOUNT: &str = "111122223333";

/// The kinds driven here, each with the name its create registers: a kind whose surface
/// words its miss with a FIXED sentence (a job), two whose sentences NAME the resource (a
/// database, a crawler), and one that answers the generic entity sentence (a connection).
const KINDS: [(&str, &str); 4] = [
    ("job", "miss-job"),
    ("database", "miss_db"),
    ("crawler", "miss-crawler"),
    ("connection", "miss-connection"),
];

/// The delete that drops the resource `name` of the kind `segment`.
fn delete_request(segment: &str, name: &str) -> (&'static str, serde_json::Value) {
    match segment {
        "job" => ("DeleteJob", json!({ "JobName": name })),
        "database" => ("DeleteDatabase", json!({ "Name": name })),
        "crawler" => ("DeleteCrawler", json!({ "Name": name })),
        "connection" => ("DeleteConnection", json!({ "ConnectionName": name })),
        other => panic!("no delete is written here for {other:?}"),
    }
}

/// Assert that `GetTags` of `arn`, served in `region`, is the miss `expected` words.
async fn assert_miss(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    region: &str,
    arn: &str,
    expected: &str,
) {
    let (status, body) = call_in_region(router, region, "GetTags", &json!({ "ResourceArn": arn }))
        .await;
    assert_eq!(
        status, 400,
        "case {case}: {arn} names no resource in {region} and must be refused, got {status} \
         with {body}"
    );
    assert!(
        error_code(&body).ends_with("EntityNotFoundException"),
        "case {case}: expected EntityNotFoundException, got {} in {body}",
        error_code(&body)
    );
    assert_eq!(
        error_message(&body),
        expected,
        "case {case}: unexpected wording for the tag read's miss of {arn}"
    );
    assert!(
        body.get("Tags").is_none(),
        "case {case}: a refused read reports no tags, got {body}"
    );
}

/// An ARN of a taggable kind that names no resource this endpoint holds is refused with the
/// miss sentence of the surface that OWNS that kind — whichever way the ARN resolves to
/// nothing.
#[tokio::test]
async fn the_tag_read_words_its_miss_as_the_kind_s_own_surface_does() {
    let router = emulator();

    // One resource of each kind exists, so every refusal below is a miss and not a broken
    // resolution.
    for (segment, name) in KINDS {
        let (operation, body) = create_request(segment, name, &json!({ "created": segment }));
        ok(&router, operation, &body).await;
    }

    for (segment, name) in KINDS {
        // A name nothing was created under.
        let absent = format!("{segment}-never-created");
        assert_miss(
            &router,
            &format!("{segment}: a name nothing was created under"),
            REGION,
            &resource_arn(segment, &absent),
            &read_miss_message(segment, &absent),
        )
        .await;

        // Another account's estate, under a name this account DOES hold: the account
        // segment is what makes it another estate.
        assert_miss(
            &router,
            &format!("{segment}: another account's ARN"),
            REGION,
            &glue_arn(REGION, OTHER_ACCOUNT, &format!("{segment}/{name}")),
            &read_miss_message(segment, name),
        )
        .await;

        // Another Region's ARN, served by this endpoint — the recorded shape of this
        // refusal — and this Region's ARN served by that endpoint: a Glue resource belongs
        // to the Region its ARN names.
        assert_miss(
            &router,
            &format!("{segment}: another Region's ARN at this endpoint"),
            REGION,
            &glue_arn(OTHER_REGION, ACCOUNT_ID, &format!("{segment}/{name}")),
            &read_miss_message(segment, name),
        )
        .await;
        assert_miss(
            &router,
            &format!("{segment}: this Region's ARN at another endpoint"),
            OTHER_REGION,
            &resource_arn(segment, name),
            &read_miss_message(segment, name),
        )
        .await;
    }

    // Every resource that exists still reports the tags its create recorded, so the misses
    // above changed nothing.
    for (segment, name) in KINDS {
        let got = read_tags(&router, segment, &resource_arn(segment, name)).await;
        assert_eq!(
            got,
            map_of(&json!({ "created": segment })),
            "case {segment}: the refused reads must leave the tag set alone, got {got:?}"
        );
    }
}

/// The resource a DELETE dropped is one this endpoint holds no longer, so its ARN earns the
/// same per-kind miss — while a sibling of that kind goes on reporting its own tags.
#[tokio::test]
async fn a_deleted_resource_s_arn_earns_its_kind_s_miss() {
    let router = emulator();

    for (segment, name) in KINDS {
        let sibling = format!("{name}-kept");
        for registered in [name, sibling.as_str()] {
            let (operation, body) =
                create_request(segment, registered, &json!({ "created": segment }));
            ok(&router, operation, &body).await;
        }

        let (operation, body) = delete_request(segment, name);
        ok(&router, operation, &body).await;

        assert_miss(
            &router,
            &format!("{segment}: a resource that was deleted"),
            REGION,
            &resource_arn(segment, name),
            &read_miss_message(segment, name),
        )
        .await;

        let got = read_tags(&router, segment, &resource_arn(segment, &sibling)).await;
        assert_eq!(
            got,
            map_of(&json!({ "created": segment })),
            "case {segment}: the sibling's tags must survive the delete, got {got:?}"
        );
    }
}

/// A name that differs only in CASE from a stored one is a miss for a kind whose names the
/// catalog does not fold, and resolves for one it does — and the sentence a miss carries
/// names the resource as the store keys that kind, not as the request spelled it.
#[tokio::test]
async fn case_decides_the_miss_for_the_kinds_the_catalog_does_not_fold() {
    let router = emulator();
    for (segment, name) in KINDS {
        let (operation, body) = create_request(segment, name, &json!({ "created": segment }));
        ok(&router, operation, &body).await;
    }

    // A job and a crawler are keyed by the name exactly as the create sent it, so another
    // casing of a stored name names no resource at all.
    for segment in ["job", "crawler"] {
        let name = KINDS
            .iter()
            .find_map(|(kind, name)| (*kind == segment).then_some(*name))
            .unwrap_or_else(|| panic!("case {segment}: the kind is one of the rows"));
        let shouted = name.to_uppercase();
        assert_miss(
            &router,
            &format!("{segment}: a stored name in another case"),
            REGION,
            &resource_arn(segment, &shouted),
            &read_miss_message(segment, &shouted),
        )
        .await;
    }

    // A database IS folded, so another casing of a stored name resolves to it — and the
    // same fold is what the miss of an absent database names.
    let (status, body) = call(
        &router,
        "GetTags",
        &json!({ "ResourceArn": resource_arn("database", "MISS_DB") }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case database: a folded name resolves whatever case it is spelled in, got {status} \
         with {body}"
    );
    assert_miss(
        &router,
        "database: an absent name spelled in another case",
        REGION,
        &resource_arn("database", "ABSENT_DB"),
        "Database absent_db not found.",
    )
    .await;
}
