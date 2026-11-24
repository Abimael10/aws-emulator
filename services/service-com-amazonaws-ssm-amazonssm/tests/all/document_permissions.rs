// SPDX-License-Identifier: CC0-1.0

//! Who an SSM document is shared with: `ModifyDocumentPermission` records the share and
//! `DescribeDocumentPermission` reports it.
//!
//! The rules under test:
//!
//! 1. A share is STATE of the document it is granted on: `ModifyDocumentPermission`
//!    persists each account the document is shared with at the version the call named (the
//!    default version when it named none), and `DescribeDocumentPermission` reports exactly
//!    those accounts — as `AccountIds` and as the `AccountSharingInfoList` pairs, which
//!    cannot disagree. A document nobody has shared reports the EMPTY list.
//! 2. Both account lists apply to the one share set, removals last: "This action has a
//!    higher priority than AccountIdsToAdd", and an account the call does not name keeps
//!    the grant it had. Every spelling of the public value names the one public grant.
//! 3. The sharing is per document and per endpoint: a share on one document changes no
//!    other document, and a name created in two Regions carries two independent share sets.
//! 4. A permission read is paged by account id, bounded by the longest account list the
//!    response can carry; a token that is not an account id was not issued by that read.
//! 5. A permission call names a document this account owns: a name nothing holds, and a
//!    document Systems Manager publishes, are refused — with the one `InvalidDocument` both
//!    the reader and the writer answer such a call with — rather than reported as shared.
//! 6. The published share quotas bound what one account shares — the accounts one document
//!    is shared privately with, and how many documents it shares publicly — and a refused
//!    call persists nothing.
//! 7. A document that is still shared cannot be deleted, and a publicly shared one belongs
//!    to the `Public` owner class a listing selects on.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call`], [`call_in_region`], [`error_code`],
//! [`error_message`]) dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. Nothing reaches into the state struct, so every read
//! follows the write it must report.
//!
//! The suites are tables. Each row is one input along the axis the rule generalises over —
//! how many accounts a share names, private against public, the version it grants, which
//! accounts a later call withdraws, the page size, the Region — rather than the single
//! share a recorded unit happened to grant.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, call_in_region, error_code, error_message, router, REGION};

/// The one permission type the model's `DocumentPermissionType` enum declares
/// (`smithy_query(query="shape", shape="DocumentPermissionType")`).
const SHARE: &str = "Share";

/// The `SharedDocumentVersion` values the modelled pattern admits
/// (`^([$]LATEST|[$]DEFAULT|[$]ALL)$`, `smithy_query(query="shape",
/// shape="SharedDocumentVersion")`).
const SHARED_VERSIONS: &[&str] = &["$LATEST", "$DEFAULT", "$ALL"];

/// The version a share that names none grants: "If it isn't specified, the system choose
/// the Default version to share" (`ModifyDocumentPermissionRequest$SharedDocumentVersion`).
const DEFAULT_SHARED_VERSION: &str = "$DEFAULT";

/// "If you share a document publicly, you must specify All as the account ID"
/// (`AccountIdsToAdd`), which the modelled `AccountId` pattern spells
/// case-insensitively (`^(?i)all|[0-9]{12}$`).
const ALL: &str = "all";

/// How many accounts ONE call may name: "You can specify a maximum of 20 accounts per API
/// operation to share a private document" (`DocumentPermissionLimit`), which is the
/// modelled `AccountIdList` length bound.
const MAX_ACCOUNTS_PER_CALL: usize = 20;

/// "By default, you can share a private document with a maximum of 1,000 accounts"
/// (`DocumentPermissionLimit`).
const MAX_SHARED_ACCOUNTS: usize =
    service_com_amazonaws_ssm_amazonssm::state::document_permissions::MAX_SHARED_ACCOUNTS;

/// "and publicly share up to five documents" (`DocumentPermissionLimit`, same sentence).
const MAX_PUBLIC_DOCUMENTS: usize =
    service_com_amazonaws_ssm_amazonssm::state::document_permissions::MAX_PUBLICLY_SHARED_DOCUMENTS;

/// A document Systems Manager publishes (`crate::state::PUBLISHED_DOCUMENTS`), which
/// resolves for a read without anybody creating it and is owned by Amazon.
const PUBLISHED_DOCUMENT: &str = "AWS-RunShellScript";

/// A minimal `Command` document: one step, no parameters, so a row differs only in the
/// sharing it is given.
const COMMAND_JSON: &str = r#"{
  "schemaVersion": "2.2",
  "description": "a document to share",
  "mainSteps": [
    {
      "action": "aws:runShellScript",
      "name": "runShell",
      "inputs": { "runCommand": ["echo hi"] }
    }
  ]
}"#;

/// One row of the share table: the case, the document to share, the accounts the call
/// names, the version it names, and the sharing a read must report afterwards.
type ShareCase = (
    &'static str,
    &'static str,
    Vec<String>,
    Option<&'static str>,
    Vec<(String, String)>,
);

/// One row of the withdrawal table: the case, the accounts the call adds, the accounts it
/// removes, and the accounts left shared.
type WithdrawalCase = (&'static str, Vec<String>, Vec<String>, Vec<String>);

/// One row of the delete-and-listing table: the case, the accounts the document is shared
/// with, and whether a listing calls it public.
type SharedDeleteCase = (&'static str, Vec<String>, bool);

/// `count` distinct account numbers, each the twelve digits the modelled `AccountId`
/// pattern admits.
fn accounts(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("{:012}", 111_111_111_111u64 + index as u64))
        .collect()
}

/// Create a document named `name` in `region`, asserting the create succeeded.
async fn create_in(router: &MultiServiceRouter, case: &str, region: &str, name: &str) {
    let (status, response) = call_in_region(
        router,
        region,
        "CreateDocument",
        &json!({ "Name": name, "Content": COMMAND_JSON }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: CreateDocument {name} in {region} must succeed, got {status} {response}"
    );
}

/// Create a document named `name` in the default Region.
async fn create(router: &MultiServiceRouter, case: &str, name: &str) {
    create_in(router, case, REGION, name).await;
}

/// `ModifyDocumentPermission` with `body`, returning `(status, body)`.
async fn modify(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "ModifyDocumentPermission", body).await
}

/// Share `name` with `to_add` at `version`, asserting the call succeeded.
async fn share(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    to_add: &[String],
    version: Option<&str>,
) {
    let mut body = json!({
        "Name": name,
        "PermissionType": SHARE,
        "AccountIdsToAdd": to_add,
    });
    if let Some(version) = version {
        body["SharedDocumentVersion"] = json!(version);
    }
    let (status, response) = modify(router, &body).await;
    assert_eq!(
        status, 200,
        "case {case}: sharing {name} with {to_add:?} must succeed, got {status} {response}"
    );
}

/// `DescribeDocumentPermission` with `body`, returning `(status, body)`.
async fn describe_permission(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "DescribeDocumentPermission", body).await
}

/// The permissions of `name`, asserting the read succeeded.
async fn permissions_of(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let (status, response) =
        describe_permission(router, &json!({ "Name": name, "PermissionType": SHARE })).await;
    assert_eq!(
        status, 200,
        "case {case}: DescribeDocumentPermission {name} must succeed, got {status} {response}"
    );
    response
}

/// The `AccountIds` a permission read reported.
fn account_ids(response: &Value) -> Vec<String> {
    response["AccountIds"]
        .as_array()
        .unwrap_or_else(|| panic!("AccountIds must be a list, got {response}"))
        .iter()
        .map(|entry| entry.as_str().unwrap_or_default().to_string())
        .collect()
}

/// The `(AccountId, SharedDocumentVersion)` pairs a permission read reported.
fn sharing(response: &Value) -> Vec<(String, String)> {
    response["AccountSharingInfoList"]
        .as_array()
        .unwrap_or_else(|| panic!("AccountSharingInfoList must be a list, got {response}"))
        .iter()
        .map(|entry| {
            (
                entry["AccountId"].as_str().unwrap_or_default().to_string(),
                entry["SharedDocumentVersion"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// RULE 1: what a share persisted is what a permission read reports — the accounts and the
/// version each one holds the document at — over the number of accounts a call names,
/// private against public sharing, and the version it grants.
#[tokio::test]
async fn a_share_round_trips_through_the_permission_read() {
    let router = router();
    let twenty = accounts(MAX_ACCOUNTS_PER_CALL);
    let three = accounts(3);

    // (case, name, accounts to share with, version the call names, expected pairs)
    let cases: Vec<ShareCase> = vec![
        (
            "no share at all",
            "UnsharedDocument",
            Vec::new(),
            None,
            Vec::new(),
        ),
        (
            "one private account, no version named",
            "OnePrivateShare",
            vec![three[0].clone()],
            None,
            vec![(three[0].clone(), DEFAULT_SHARED_VERSION.to_string())],
        ),
        (
            "three private accounts at the latest version",
            "ThreePrivateShares",
            three.clone(),
            Some("$LATEST"),
            three
                .iter()
                .map(|account| (account.clone(), "$LATEST".to_string()))
                .collect(),
        ),
        (
            "the public value, which is reported in its one spelling",
            "PublicShare",
            vec!["All".to_string()],
            None,
            vec![(ALL.to_string(), DEFAULT_SHARED_VERSION.to_string())],
        ),
        (
            "the public value beside a private account, at every version",
            "PublicAndPrivateShare",
            vec!["ALL".to_string(), three[1].clone()],
            Some("$ALL"),
            vec![
                (three[1].clone(), "$ALL".to_string()),
                (ALL.to_string(), "$ALL".to_string()),
            ],
        ),
        (
            "the most accounts one call may name",
            "TwentyPrivateShares",
            twenty.clone(),
            Some("$DEFAULT"),
            twenty
                .iter()
                .map(|account| (account.clone(), "$DEFAULT".to_string()))
                .collect(),
        ),
    ];

    for (case, name, to_add, version, expected) in cases {
        create(&router, case, name).await;
        if !to_add.is_empty() {
            share(&router, case, name, &to_add, version).await;
        }

        // A page holds at most twenty accounts, which every row here fits in.
        let response = permissions_of(&router, case, name).await;
        let mut reported = sharing(&response);
        reported.sort();
        let mut wanted = expected.clone();
        wanted.sort();
        assert_eq!(
            reported, wanted,
            "case {case}: expected the sharing {wanted:?}, got {reported:?}"
        );

        // The two projections of the one share set agree, member for member.
        let mut ids = account_ids(&response);
        ids.sort();
        let mut wanted_ids: Vec<String> = wanted.iter().map(|(id, _)| id.clone()).collect();
        wanted_ids.sort();
        assert_eq!(
            ids, wanted_ids,
            "case {case}: expected the account ids {wanted_ids:?}, got {ids:?}"
        );
        assert_eq!(
            response["NextToken"],
            Value::Null,
            "case {case}: one page holds every account, got {response}"
        );
    }
}

/// RULE 1, the version half: each value the modelled `SharedDocumentVersion` admits is the
/// version the read reports, a later share of the same account moves it, and a value
/// outside that set is refused.
#[tokio::test]
async fn a_share_grants_the_version_the_call_named() {
    let router = router();
    let account = accounts(1);
    let name = "VersionedShare";
    create(&router, "versions", name).await;

    for version in SHARED_VERSIONS {
        share(&router, version, name, &account, Some(version)).await;
        let reported = sharing(&permissions_of(&router, version, name).await);
        assert_eq!(
            reported,
            vec![(account[0].clone(), version.to_string())],
            "case {version}: a re-share must move the account to that version, got {reported:?}"
        );
    }

    // One past the published set: a version selector the pattern does not admit is refused
    // at the boundary, and the grant the document already holds is untouched.
    let (status, response) = modify(
        &router,
        &json!({
            "Name": name,
            "PermissionType": SHARE,
            "AccountIdsToAdd": account,
            "SharedDocumentVersion": "$NONE",
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case a version outside the pattern: must be refused, got {status} {response}"
    );
    let reported = sharing(&permissions_of(&router, "after the refusal", name).await);
    assert_eq!(
        reported,
        vec![(account[0].clone(), "$ALL".to_string())],
        "case after the refusal: the last granted version stands, got {reported:?}"
    );
}

/// RULE 2: both lists apply to the one share set with removals last, and an account the
/// call does not name keeps the grant it had.
#[tokio::test]
async fn a_removal_outranks_an_add_and_clears_only_what_it_names() {
    let router = router();
    let held = accounts(3);

    // (case, accounts to add, accounts to remove, the accounts left shared)
    let cases: Vec<WithdrawalCase> = vec![
        (
            "one of the three withdrawn",
            Vec::new(),
            vec![held[0].clone()],
            vec![held[1].clone(), held[2].clone()],
        ),
        (
            "an account named in both lists is removed",
            vec![accounts(5)[4].clone()],
            vec![accounts(5)[4].clone()],
            held.clone(),
        ),
        (
            "an account that was never shared",
            Vec::new(),
            vec!["999988887777".to_string()],
            held.clone(),
        ),
        (
            "the public grant withdrawn under another spelling",
            vec!["all".to_string()],
            vec!["ALL".to_string()],
            held.clone(),
        ),
        (
            "an add and a removal in one call",
            vec!["444455556666".to_string()],
            vec![held[2].clone()],
            vec![
                held[0].clone(),
                held[1].clone(),
                "444455556666".to_string(),
            ],
        ),
        (
            "every account withdrawn",
            Vec::new(),
            held.clone(),
            Vec::new(),
        ),
    ];

    for (index, (case, to_add, to_remove, left)) in cases.into_iter().enumerate() {
        // Each row starts from the same three shared accounts, on its own document.
        let name = format!("WithdrawCase{index}");
        create(&router, case, &name).await;
        share(&router, case, &name, &held, None).await;

        let (status, response) = modify(
            &router,
            &json!({
                "Name": name,
                "PermissionType": SHARE,
                "AccountIdsToAdd": to_add,
                "AccountIdsToRemove": to_remove,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: the change must succeed, got {status} {response}"
        );

        let mut reported = account_ids(&permissions_of(&router, case, &name).await);
        reported.sort();
        let mut wanted = left.clone();
        wanted.sort();
        assert_eq!(
            reported, wanted,
            "case {case}: expected {wanted:?} left shared, got {reported:?}"
        );
    }

    // A call that names no account in either list asks for no change this operation can
    // make, and the share already held stands.
    let name = "NoAccountsNamed";
    create(&router, "neither list", name).await;
    share(&router, "neither list", name, &held, None).await;
    let (status, response) = modify(
        &router,
        &json!({ "Name": name, "PermissionType": SHARE }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case neither list: must be refused, got {status} {response}"
    );
    assert_eq!(
        error_code(&response),
        "InvalidParameterException",
        "case neither list: expected an InvalidParameterException, got {response}"
    );
    let mut reported = account_ids(&permissions_of(&router, "neither list", name).await);
    reported.sort();
    assert_eq!(
        reported,
        {
            let mut held = held.clone();
            held.sort();
            held
        },
        "case neither list: the share already held stands, got {reported:?}"
    );
}

/// RULE 3: the sharing belongs to ONE document reached through ONE endpoint — a share on
/// one document changes no other, and a name created in two Regions carries two share sets.
#[tokio::test]
async fn sharing_is_held_per_document_and_per_region() {
    let router = router();
    let other_region = "eu-west-1";
    let shared = accounts(2);

    create(&router, "two documents", "FirstDocument").await;
    create(&router, "two documents", "SecondDocument").await;
    share(&router, "two documents", "FirstDocument", &shared[..1], None).await;

    assert_eq!(
        account_ids(&permissions_of(&router, "the shared document", "FirstDocument").await),
        vec![shared[0].clone()],
        "case the shared document: must report its own share"
    );
    assert_eq!(
        account_ids(&permissions_of(&router, "the other document", "SecondDocument").await),
        Vec::<String>::new(),
        "case the other document: a share elsewhere shares nothing here"
    );

    // The same name in another Region is another document, with its own sharing.
    create_in(&router, "another Region", other_region, "FirstDocument").await;
    let (status, response) = call_in_region(
        &router,
        other_region,
        "ModifyDocumentPermission",
        &json!({
            "Name": "FirstDocument",
            "PermissionType": SHARE,
            "AccountIdsToAdd": [shared[1].clone()],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case another Region: the share must succeed, got {status} {response}"
    );
    let (status, elsewhere) = call_in_region(
        &router,
        other_region,
        "DescribeDocumentPermission",
        &json!({ "Name": "FirstDocument", "PermissionType": SHARE }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case another Region: the read must succeed, got {status} {elsewhere}"
    );
    assert_eq!(
        account_ids(&elsewhere),
        vec![shared[1].clone()],
        "case another Region: must report the share made through that endpoint"
    );
    assert_eq!(
        account_ids(&permissions_of(&router, "the first Region", "FirstDocument").await),
        vec![shared[0].clone()],
        "case the first Region: unchanged by the share made elsewhere"
    );
}

/// RULE 4: a permission read is paged by account id — every page size reports each account
/// exactly once over the pages it hands out, and a token it never issued is refused.
#[tokio::test]
async fn a_permission_read_pages_by_account_id() {
    let router = router();
    let shared = accounts(5);
    let name = "PagedShares";
    create(&router, "paging", name).await;
    share(&router, "paging", name, &shared, None).await;
    let mut wanted = shared.clone();
    wanted.sort();

    // The page sizes a read may ask for: the modelled lower bound, one between, the page
    // the whole share set fits in, and the modelled upper bound — which the longest
    // account list a response can carry bounds to twenty.
    for max_results in [Some(1), Some(2), Some(5), Some(200), None] {
        let case = format!("MaxResults={max_results:?}");
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        // One more pass than there are accounts cannot be needed; the bound keeps a
        // non-advancing read from looping.
        for _ in 0..=shared.len() {
            let mut body = json!({ "Name": name, "PermissionType": SHARE });
            if let Some(max_results) = max_results {
                body["MaxResults"] = json!(max_results);
            }
            if let Some(token) = &token {
                body["NextToken"] = json!(token);
            }
            let (status, response) = describe_permission(&router, &body).await;
            assert_eq!(
                status, 200,
                "case {case}: the page must be served, got {status} {response}"
            );
            let page = account_ids(&response);
            let wanted_len = max_results.unwrap_or(20).min(20) as usize;
            assert!(
                !page.is_empty() && page.len() <= wanted_len,
                "case {case}: a page holds 1..={wanted_len} accounts, got {page:?}"
            );
            walked.extend(page);
            token = response["NextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            token, None,
            "case {case}: the walk must end without a token, got {token:?}"
        );
        let mut reported = walked.clone();
        reported.sort();
        assert_eq!(
            reported, wanted,
            "case {case}: every account exactly once; expected {wanted:?}, got {walked:?}"
        );
    }

    // A token that is not an account id, and one naming an account this document is not
    // shared with, were both issued by no page of this read.
    for token in [
        "not-a-token",
        "12",
        "arn:aws:ssm:::document/PagedShares",
        "999988887777",
    ] {
        let (status, response) = describe_permission(
            &router,
            &json!({ "Name": name, "PermissionType": SHARE, "NextToken": token }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case token {token:?}: must be refused, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidNextToken",
            "case token {token:?}: expected InvalidNextToken, got {response}"
        );
    }
}

/// RULE 5: a permission call names a document this account owns — anything else is refused
/// rather than reported as sharing nothing.
#[tokio::test]
async fn a_permission_call_names_a_document_this_account_owns() {
    let router = router();
    create(&router, "a document to compare against", "OwnedDocument").await;

    // (case, operation, body, the error code the refusal carries)
    let cases: Vec<(&str, &str, Value, &str)> = vec![
        (
            "a read of a name nothing holds",
            "DescribeDocumentPermission",
            json!({ "Name": "NoSuchDocument", "PermissionType": SHARE }),
            "InvalidDocument",
        ),
        (
            "a share of a name nothing holds",
            "ModifyDocumentPermission",
            json!({
                "Name": "NoSuchDocument",
                "PermissionType": SHARE,
                "AccountIdsToAdd": ["111122223333"],
            }),
            "InvalidDocument",
        ),
        (
            "a read of a document Systems Manager publishes",
            "DescribeDocumentPermission",
            json!({ "Name": PUBLISHED_DOCUMENT, "PermissionType": SHARE }),
            "InvalidDocument",
        ),
        (
            "a share of a document Systems Manager publishes",
            "ModifyDocumentPermission",
            json!({
                "Name": PUBLISHED_DOCUMENT,
                "PermissionType": SHARE,
                "AccountIdsToAdd": ["111122223333"],
            }),
            "InvalidDocument",
        ),
        (
            "a read naming a permission type this API has not",
            "DescribeDocumentPermission",
            json!({ "Name": "OwnedDocument", "PermissionType": "Execute" }),
            "ValidationException",
        ),
        (
            "a share naming a permission type this API has not",
            "ModifyDocumentPermission",
            json!({
                "Name": "OwnedDocument",
                "PermissionType": "Execute",
                "AccountIdsToAdd": ["111122223333"],
            }),
            "ValidationException",
        ),
    ];

    for (case, operation, body, code) in cases {
        let (status, response) = call(&router, operation, &body).await;
        assert_eq!(
            status, 400,
            "case {case}: must be refused with 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            code,
            "case {case}: expected {code}, got {response}"
        );
        assert!(
            !error_message(&response).is_empty(),
            "case {case}: the refusal must say why, got {response}"
        );
    }

    // None of those refusals shared anything: the document this account owns is still
    // shared with nobody.
    assert_eq!(
        account_ids(&permissions_of(&router, "after the refusals", "OwnedDocument").await),
        Vec::<String>::new(),
        "case after the refusals: nothing was persisted"
    );
}

/// RULE 6: the published quotas bound what one account shares — and the call that trips one
/// persists nothing.
#[tokio::test]
async fn the_published_quotas_bound_what_one_account_shares() {
    let router = router();

    // Up to five documents share publicly; the sixth public share is past the quota.
    for index in 0..MAX_PUBLIC_DOCUMENTS {
        let name = format!("PublicDocument{index}");
        create(&router, "the public quota", &name).await;
        share(&router, "the public quota", &name, &["All".to_string()], None).await;
        // Re-sharing a document already public is no NEW public share.
        share(&router, "re-shared publicly", &name, &["all".to_string()], None).await;
    }
    let past = format!("PublicDocument{MAX_PUBLIC_DOCUMENTS}");
    create(&router, "past the public quota", &past).await;
    let (status, response) = modify(
        &router,
        &json!({
            "Name": past,
            "PermissionType": SHARE,
            "AccountIdsToAdd": ["All"],
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case past the public quota: must be refused, got {status} {response}"
    );
    assert_eq!(
        error_code(&response),
        "DocumentPermissionLimit",
        "case past the public quota: expected DocumentPermissionLimit, got {response}"
    );
    assert_eq!(
        account_ids(&permissions_of(&router, "past the public quota", &past).await),
        Vec::<String>::new(),
        "case past the public quota: the refused share persisted nothing"
    );
    // A PRIVATE share of that same document is still within every quota.
    share(
        &router,
        "private share past the public quota",
        &past,
        &["111122223333".to_string()],
        None,
    )
    .await;
    assert_eq!(
        account_ids(&permissions_of(&router, "private share", &past).await),
        vec!["111122223333".to_string()],
        "case private share: the private grant stands"
    );

    // One document shares privately with up to a thousand accounts; the next one is past
    // the quota.
    let name = "ManyPrivateShares";
    create(&router, "the private quota", name).await;
    let many = accounts(MAX_SHARED_ACCOUNTS + 1);
    for batch in many[..MAX_SHARED_ACCOUNTS].chunks(MAX_ACCOUNTS_PER_CALL) {
        share(&router, "the private quota", name, batch, None).await;
    }
    let (status, response) = modify(
        &router,
        &json!({
            "Name": name,
            "PermissionType": SHARE,
            "AccountIdsToAdd": [many[MAX_SHARED_ACCOUNTS].clone()],
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case past the private quota: must be refused, got {status} {response}"
    );
    assert_eq!(
        error_code(&response),
        "DocumentPermissionLimit",
        "case past the private quota: expected DocumentPermissionLimit, got {response}"
    );

    // The account the refused call named is shared nothing, and the quota's worth still is:
    // the read walks its pages to count them.
    let mut walked = 0usize;
    let mut token: Option<String> = None;
    loop {
        let mut body = json!({ "Name": name, "PermissionType": SHARE });
        if let Some(token) = &token {
            body["NextToken"] = json!(token);
        }
        let (status, page) = describe_permission(&router, &body).await;
        assert_eq!(
            status, 200,
            "case counting the shares: the page must be served, got {status} {page}"
        );
        let ids = account_ids(&page);
        assert!(
            !ids.contains(&many[MAX_SHARED_ACCOUNTS]),
            "case counting the shares: the refused account must not be shared, got {ids:?}"
        );
        walked += ids.len();
        token = page["NextToken"].as_str().map(str::to_string);
        if token.is_none() {
            break;
        }
    }
    assert_eq!(
        walked, MAX_SHARED_ACCOUNTS,
        "case counting the shares: expected {MAX_SHARED_ACCOUNTS} accounts shared, got {walked}"
    );
}

/// RULE 7: a document that is still shared cannot be deleted, and a publicly shared one is
/// what the `Public` owner class of a listing selects.
#[tokio::test]
async fn a_share_blocks_a_delete_and_joins_the_public_owner_class() {
    let router = router();
    let private = accounts(1);

    // (case, the accounts the document is shared with, whether the listing calls it public)
    let cases: Vec<SharedDeleteCase> = vec![
        ("a private share", private.clone(), false),
        ("a public share", vec!["All".to_string()], true),
        (
            "both at once",
            vec!["All".to_string(), private[0].clone()],
            true,
        ),
    ];

    for (case, shared_with, public) in cases {
        let name = format!("Blocked{}{}", shared_with.len(), u8::from(public));
        create(&router, case, &name).await;
        share(&router, case, &name, &shared_with, None).await;

        // "You attempted to delete a document while it is still shared. You must stop
        // sharing the document before you can delete it" (`InvalidDocumentOperation`).
        let (status, response) = call(&router, "DeleteDocument", &json!({ "Name": name })).await;
        assert_eq!(
            status, 400,
            "case {case}: the delete must be refused, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidDocumentOperation",
            "case {case}: expected InvalidDocumentOperation, got {response}"
        );

        // A publicly shared document is in the `Public` owner class; a privately shared
        // one is not. Either way it is still one of the documents this account holds.
        for (owner, holds) in [("Public", public), ("Self", true)] {
            let (status, listing) = call(
                &router,
                "ListDocuments",
                &json!({ "Filters": [{ "Key": "Owner", "Values": [owner] }] }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: ListDocuments Owner={owner} must succeed, got {status} {listing}"
            );
            let listed: Vec<String> = listing["DocumentIdentifiers"]
                .as_array()
                .unwrap_or_else(|| panic!("DocumentIdentifiers must be a list, got {listing}"))
                .iter()
                .map(|entry| entry["Name"].as_str().unwrap_or_default().to_string())
                .collect();
            assert_eq!(
                listed.contains(&name),
                holds,
                "case {case}: Owner={owner} must {} {name}, got {listed:?}",
                if holds { "name" } else { "not name" }
            );
        }

        // Withdrawing every share frees the document to be deleted, and the name is gone.
        let (status, response) = modify(
            &router,
            &json!({
                "Name": name,
                "PermissionType": SHARE,
                "AccountIdsToRemove": shared_with,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: the withdrawal must succeed, got {status} {response}"
        );
        let (status, response) = call(&router, "DeleteDocument", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {case}: the delete must succeed once unshared, got {status} {response}"
        );
        let (status, response) =
            describe_permission(&router, &json!({ "Name": name, "PermissionType": SHARE })).await;
        assert_eq!(
            status, 400,
            "case {case}: the deleted document names nothing, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidDocument",
            "case {case}: expected InvalidDocument, got {response}"
        );
    }

    // The published catalogue is Amazon's, so nothing this account shared made it public.
    let (status, listing) = call(
        &router,
        "ListDocuments",
        &json!({ "Filters": [{ "Key": "Owner", "Values": ["Public"] }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case no document shared publicly: the listing must succeed, got {status} {listing}"
    );
    assert_eq!(
        listing["DocumentIdentifiers"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        0,
        "case no document shared publicly: expected no document, got {listing}"
    );
}
