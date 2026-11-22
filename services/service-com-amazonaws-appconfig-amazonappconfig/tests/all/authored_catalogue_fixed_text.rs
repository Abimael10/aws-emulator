// SPDX-License-Identifier: CC0-1.0

//! The published text of AppConfig's AWS-authored extension catalogue, as FIXED text.
//!
//! The rule under test: an AWS-authored extension's published prose — its `Name` and its
//! `Description` — is AppConfig's own fixed text, keyed by the extension's name and
//! version and by nothing else, so no part of it is read off the request or off the
//! account's state: every reader reports a given entry's sentence byte for byte, whatever
//! Region the caller signed for, whatever page size the listing is walked at, however many
//! times it is read, and whatever extensions the account has created of its own. The only
//! environment-specific part of a published entry is its ARN, whose Region field follows
//! the Region the request was signed for while the account field stays empty, because no
//! account owns an extension AppConfig authors.
//!
//! The rows are the whole published catalogue
//! ([`crate::aws_authored_extension_catalog::PUBLISHED_CATALOGUE`], read from there rather
//! than hand-copied a third time), and the cases within a row differ along what the rule
//! generalises over: which Region the request is signed for (four, including the default
//! the emulator falls back to), which page size the catalogue is walked at, which reader
//! answers (the listing, `GetExtension` by id, `GetExtension` by the ARN of the version),
//! and what the account itself holds (nothing, extensions of its own, and one carrying a
//! paraphrase of a published sentence under a name shaped like a published one).
//!
//! The deployment-notification rows are additionally decomposed here into the clause that
//! is their OWN and the remainder they share ([`NOTIFICATION_OPENINGS`]), because what each
//! of those extensions delivers and where differs per entry while the rest of the sentence
//! does not: a catalogue whose row borrowed a sibling's opening, or generified it, fails on
//! that table rather than only in a replay.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_appconfig_amazonappconfig::ops::types::ACTION_POINT_VALUES;
use service_com_amazonaws_appconfig_amazonappconfig::state::{
    AWS_AUTHORED_EXTENSION_VERSION, DEFAULT_REGION, MAX_LIST_PAGE_SIZE,
};

use crate::appconfig_workflow::router;
use crate::aws_authored_extension_catalog::PUBLISHED_CATALOGUE;
use crate::extensions::{actions, create_extension};
use crate::list_extensions::walk;
use crate::resource_tags::pct;

/// The Regions a caller signs for, as the credential scope of a SigV4 `Authorization`
/// header carries them. The default the emulator falls back to is one of them, and so is
/// the Region of the recorded catalogue read this suite grew from; the others differ in
/// length and in how many hyphen-separated parts they carry, so a published sentence that
/// was interpolated with any part of the Region fails here.
const SIGNED_REGIONS: &[&str] = &[DEFAULT_REGION, "us-west-2", "eu-central-1", "ap-southeast-4"];

/// The clause AppConfig's published sentence OPENS with for each deployment-notification
/// extension: what that extension delivers and where it delivers it.
///
/// Hand-written from AppConfig's published catalogue text — the same source
/// [`PUBLISHED_CATALOGUE`] is written from — so a row that borrowed a sibling's opening, or
/// replaced it with a generified one, fails here. The three differ in both halves: events
/// to EventBridge itself, events to the topic the association configures, messages to the
/// queue the association configures.
const NOTIFICATION_OPENINGS: &[(&str, &str)] = &[
    (
        "AWS.AppConfig.DeploymentNotificationsToEventBridge",
        "Sends events to Amazon EventBridge",
    ),
    (
        "AWS.AppConfig.DeploymentNotificationsToSns",
        "Sends events to the configured Amazon SNS topic",
    ),
    (
        "AWS.AppConfig.DeploymentNotificationsToSqs",
        "Sends messages to the configured Amazon SQS queue",
    ),
];

/// The remainder every deployment-notification sentence shares after its own opening
/// clause: when the extension acts, and what it can be associated to. Written from the
/// same published text as [`NOTIFICATION_OPENINGS`].
const NOTIFICATION_REMAINDER: &str =
    "when a deployment of configuration data in AppConfig is started, completed, or rolled \
     back. Can be associated to the following resources in AppConfig: Application, \
     Environment, Configuration Profile.";

/// How many pages one walk of this catalogue may take before the walk is called broken.
const PAGE_WALK_LIMIT: usize = MAX_LIST_PAGE_SIZE * 4 + 2;

/// A `GET` against this service signed for `region`, answering the JSON body of a 200.
///
/// The Region a response's ARNs carry is read from the caller's own SigV4 credential
/// scope, so a case that varies the Region varies that scope — and the `Host` header with
/// it, the way a client configured for a Region reaches AppConfig there.
async fn get_json(router: &MultiServiceRouter, case: &str, region: &str, uri: &str) -> Value {
    let host = format!("appconfig.{region}.amazonaws.com");
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("http://{host}{uri}"))
        .header("host", host.clone())
        .header("content-type", "application/json")
        // Neither the key id nor the signature is a credential: this target runs with
        // request authentication off, so only the scope's Region and service are read.
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{region}/appconfig/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::new())
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = response.into_body();
    assert_eq!(
        status,
        200,
        "case {case:?}: expected 200, got {status} / {}",
        String::from_utf8_lossy(&body)
    );
    serde_json::from_slice(&body).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: expected a JSON body, got {error}: {}",
            String::from_utf8_lossy(&body)
        )
    })
}

/// Walk the whole listing signed for `region`, a page of `page_size` at a time, and answer
/// the entries in the order they were served.
async fn walk_in_region(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    page_size: Option<i32>,
) -> Vec<Value> {
    let mut collected: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..=PAGE_WALK_LIMIT {
        let mut query: Vec<String> = Vec::new();
        if let Some(size) = page_size {
            query.push(format!("max_results={size}"));
        }
        if let Some(token) = &token {
            query.push(format!("next_token={token}"));
        }
        let uri = if query.is_empty() {
            "/extensions".to_string()
        } else {
            format!("/extensions?{}", query.join("&"))
        };
        let body = get_json(router, case, region, &uri).await;
        collected.extend(
            body.get("Items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        token = body
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            return collected;
        }
    }
    panic!("case {case:?}: the paginated walk did not terminate");
}

/// The listed entry carrying `id`.
fn entry<'a>(case: &str, listed: &'a [Value], id: &str) -> &'a Value {
    listed
        .iter()
        .find(|item| item.get("Id").and_then(Value::as_str) == Some(id))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the listing must report {id}, got {listed:?}")
        })
}

/// A string member of a reported extension.
fn text(case: &str, reported: &Value, member: &str) -> String {
    reported
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member}, got {reported}"))
        .to_string()
}

/// The `Description` every entry of a listing reports, by `Id`: what two readings of the
/// catalogue are compared as. An entry reporting none — which an extension the account
/// created without one does — is held as `null`, so a sentence that appeared or vanished
/// between two readings is a difference rather than a panic.
fn descriptions(case: &str, listed: &[Value]) -> BTreeMap<String, Value> {
    listed
        .iter()
        .map(|item| {
            (
                text(case, item, "Id"),
                item.get("Description").cloned().unwrap_or(Value::Null),
            )
        })
        .collect()
}

/// A published entry's prose is the same text in every Region a caller signs for, while its
/// ARN follows that Region — and every reader of the entry reports that one sentence.
///
/// Rows: the whole published catalogue. Cases within a row: four signed Regions, a walk
/// that cuts the catalogue into pages of two and one that asks for no page size at all, and
/// the three readers a client reaches an entry through.
#[tokio::test]
async fn a_published_entry_carries_the_same_text_in_every_signed_region() {
    let router = router();
    let mut first_reading: Option<BTreeMap<String, Value>> = None;

    for region in SIGNED_REGIONS {
        for page_size in [Some(2), None] {
            let case = format!("signed for {region} at page size {page_size:?}");
            let listed = walk_in_region(&router, &case, region, page_size).await;

            for published in PUBLISHED_CATALOGUE {
                let case = format!("{case}, {}", published.id);
                let summary = entry(&case, &listed, published.id);
                assert_eq!(
                    text(&case, summary, "Description"),
                    published.description,
                    "case {case:?}: expected the published sentence, got {summary}"
                );
                assert_eq!(
                    text(&case, summary, "Name"),
                    published.name,
                    "case {case:?}: expected the published name, got {summary}"
                );

                // The ARN is the one part of the entry the request shapes: its Region is
                // the Region the caller signed for, its account field stays empty, and its
                // resource portion names the published version.
                let served_arn = text(&case, summary, "Arn");
                let fields: Vec<&str> = served_arn.splitn(6, ':').collect();
                assert_eq!(
                    fields.len(),
                    6,
                    "case {case:?}: an ARN has six fields, got {served_arn}"
                );
                assert_eq!(
                    (fields[2], fields[3], fields[4], fields[5]),
                    (
                        "appconfig",
                        *region,
                        "",
                        format!(
                            "extension/{}/{AWS_AUTHORED_EXTENSION_VERSION}",
                            published.id
                        )
                        .as_str()
                    ),
                    "case {case:?}: expected the published version's ARN in the signed \
                     Region, owned by no account, got {served_arn}"
                );

                // Every reader of the entry reports that same sentence: the listing above,
                // and `GetExtension` addressed by the published id and by the ARN the
                // listing just served.
                for (form, identifier) in [
                    ("by id", published.id.to_string()),
                    ("by ARN", served_arn.clone()),
                ] {
                    let read = get_json(
                        &router,
                        &case,
                        region,
                        &format!("/extensions/{}", pct(&identifier)),
                    )
                    .await;
                    assert_eq!(
                        text(&case, &read, "Description"),
                        published.description,
                        "case {case:?} {form}: expected the published sentence, got {read}"
                    );
                }
            }

            // Byte for byte the reading the first case got, so a sentence that varied with
            // the Region or the page size fails as a whole catalogue rather than per row.
            let reading = descriptions(&case, &listed);
            match &first_reading {
                None => first_reading = Some(reading),
                Some(expected) => assert_eq!(
                    &reading, expected,
                    "case {case:?}: the published text may not vary with the request"
                ),
            }
        }
    }
}

/// Each deployment-notification entry opens on what IT delivers and where, and shares only
/// the remainder of the sentence with its siblings.
///
/// Rows: all three published notification extensions, which differ in both halves of their
/// opening clause. Within a row: the full sentence is its own opening plus the shared
/// remainder, and no sibling's opening fits it.
#[tokio::test]
async fn each_notification_entry_opens_on_what_it_delivers_and_where() {
    let router = router();
    let listed = walk(&router, "the published catalogue", None).await;

    for (id, opening) in NOTIFICATION_OPENINGS {
        let case = *id;
        let served = text(case, entry(case, &listed, id), "Description");
        assert_eq!(
            served,
            format!("{opening} {NOTIFICATION_REMAINDER}"),
            "case {case:?}: expected this entry's own opening clause {opening:?} followed by \
             the shared remainder, got {served:?}"
        );
        for (sibling, other) in NOTIFICATION_OPENINGS {
            if sibling == id {
                continue;
            }
            assert!(
                !served.starts_with(other),
                "case {case:?}: expected its own opening clause, got {sibling}'s: {served:?}"
            );
        }
    }

    // The openings are what tells the three entries apart, so no two of them may be the
    // same text.
    let distinct: std::collections::BTreeSet<&str> = NOTIFICATION_OPENINGS
        .iter()
        .map(|(_, opening)| *opening)
        .collect();
    assert_eq!(
        distinct.len(),
        NOTIFICATION_OPENINGS.len(),
        "each notification entry opens on its own clause, got {distinct:?}"
    );
}

/// The published text is not the account's: extensions the account creates — including one
/// whose own description paraphrases a published sentence, under a name shaped like a
/// published one — leave every published sentence exactly as a fresh account reads it, and
/// reading the catalogue again does not change it either.
///
/// Rows: the published catalogue, read against a store holding nothing, then holding the
/// account's own extensions, then after a second version was written. Cases within a row:
/// the page size the catalogue is walked at (one entry per page, the published maximum, and
/// none at all).
#[tokio::test]
async fn the_accounts_own_extensions_do_not_change_the_published_text() {
    let router = router();
    let baseline = descriptions(
        "a fresh account",
        &walk(&router, "a fresh account", None).await,
    );
    assert_eq!(
        baseline.len(),
        PUBLISHED_CATALOGUE.len(),
        "a fresh account reads the published catalogue, got {baseline:?}"
    );

    // The account's own extensions, written through the front door: one carrying a
    // paraphrase of a published sentence under a name shaped like a published one, one
    // carrying no description at all, and one carrying a sentence of its own.
    let paraphrase = "Sends events to an Amazon SQS queue when a deployment of configuration \
                      data in AppConfig is started, completed, or rolled back.";
    let own: &[(&str, Option<&str>)] = &[
        (
            "AppConfig deployment events to Amazon SQS copy",
            Some(paraphrase),
        ),
        ("no-description-of-its-own", None),
        ("my own extension", Some("Whatever this account wrote here")),
    ];
    for (name, description) in own {
        let mut body = json!({
            "Name": name,
            "Actions": actions(&[ACTION_POINT_VALUES[0]]),
        });
        if let Some(description) = description {
            body["Description"] = Value::from(*description);
        }
        let created = create_extension(&router, &body, None).await;
        assert_eq!(
            created.status,
            201,
            "case {name:?}: CreateExtension expected 201, got {} / {}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );
    }
    // A second version of the account's own extension, so the listing reports a record it
    // has rewritten since the baseline.
    let revised = create_extension(
        &router,
        &json!({
            "Name": "my own extension",
            "Description": paraphrase,
            "Actions": actions(&[ACTION_POINT_VALUES[0]]),
        }),
        Some(1),
    )
    .await;
    assert_eq!(
        revised.status,
        201,
        "a second version of the account's own extension expected 201, got {} / {}",
        revised.status,
        String::from_utf8_lossy(&revised.body)
    );

    for page_size in [Some(1), Some(MAX_LIST_PAGE_SIZE as i32), None] {
        let case = format!("the account's own extensions, page size {page_size:?}");
        let listed = walk(&router, &case, page_size).await;
        let served = descriptions(&case, &listed);
        for (id, published) in &baseline {
            assert_eq!(
                served.get(id),
                Some(published),
                "case {case:?}: expected {id} to carry the sentence a fresh account reads, \
                 got {served:?}"
            );
        }
        // The account's own text stays the account's: the two halves of the catalogue do
        // not bleed into one another.
        for (name, description) in own {
            let mine = listed
                .iter()
                .find(|item| item.get("Name").and_then(Value::as_str) == Some(*name))
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the listing must report {name:?}, got {listed:?}")
                });
            let expected = if *name == "my own extension" {
                Some(paraphrase)
            } else {
                *description
            };
            assert_eq!(
                mine.get("Description").and_then(Value::as_str),
                expected,
                "case {case:?}: expected {name:?} to carry what this account wrote, got {mine}"
            );
        }

        // A second reading reports the same text, so reporting the catalogue does not
        // rewrite it.
        let again = descriptions(&case, &walk(&router, &case, page_size).await);
        assert_eq!(
            again, served,
            "case {case:?}: reading the catalogue must not change what it serves"
        );
    }
}
