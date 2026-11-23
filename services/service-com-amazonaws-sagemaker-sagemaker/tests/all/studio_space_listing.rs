// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for the Studio SPACE LISTING — `ListSpaces` — and for the tag
//! operations over a Studio DOMAIN, which reach the tag set its own `CreateDomain` stored.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`, `awsJson1_1`)
//! through the same `MultiServiceRouter` the production router uses, so what these tests
//! exercise is what a real SDK client gets. The harness (`Emulator`, `error_code`, `message`,
//! `str_field`) is the one `inference_deployment_lifecycle` already defines for this target,
//! and the domain helpers are `studio_user_profiles`'.
//!
//! The rules under test:
//!
//! * **`ListSpaces` reports the spaces the store holds, narrowed by the search members the
//!   request names, ordered by the instant it asked for, and paged by `MaxResults` with a
//!   `NextToken` that resumes exactly where the page stopped** — and each entry summarises the
//!   settings its own create stored. The rows span what that rule generalises over: which
//!   search member narrows the page (the domain, the name fragment, both, neither), the sort
//!   key and the sort order, and the page size, with the result computed from what each create
//!   carried rather than read back out of the answer being checked.
//! * **a tag a resource's own create stored is the tag set the tag operations act on**, which
//!   for a domain is what `ListTags` reports, what `AddTags` adds to and what `DeleteTags`
//!   removes from — and an ARN of that family this account holds no domain under is refused in
//!   the domain family's own words.

use std::time::Instant;

use serde_json::{json, Value};
use service_com_amazonaws_sagemaker_sagemaker::state::{
    SPACE_STATUS_IN_SERVICE, SPACE_STATUS_PENDING,
};

use crate::inference_deployment_lifecycle::{error_code, str_field, Emulator, ACCOUNT};
use crate::studio_user_profiles::{
    minimal_domain_with_auth, wait_for_domain_in_service, POLL_INTERVAL, SETTLE_DEADLINE,
};

/// The Region every case here signs for — the one the shared harness's own `call` and `ok`
/// sign for — and so the Region the ARNs it asserts carry.
const REGION: &str = "us-east-1";

/// One space a case creates: the domain it belongs to, its name, and the settings the create
/// carries beyond the pair that identifies it.
struct Declared {
    domain: usize,
    name: &'static str,
    extra: Value,
}

/// One row of the listing table: the case, the `ListSpaces` request it makes, and the
/// `(DomainId, SpaceName)` pairs the page must carry, in that order.
type ListingRow = (&'static str, Value, Vec<(String, String)>);

/// The spaces the listing table is built over: two domains, four spaces, each carrying a
/// different part of the settings surface so the summaries and the filters are both exercised.
fn declared() -> Vec<Declared> {
    vec![
        Declared {
            domain: 0,
            name: "alpha-analysis",
            extra: json!({
                "SpaceDisplayName": "Alpha analysis",
                "SpaceSharingSettings": { "SharingType": "Private" },
                "SpaceSettings": { "AppType": "JupyterLab" },
            }),
        },
        Declared {
            domain: 0,
            name: "beta-analysis",
            extra: json!({
                "SpaceSettings": {
                    "AppType": "CodeEditor",
                    "RemoteAccess": "ENABLED",
                    "SpaceStorageSettings": {
                        "EbsStorageSettings": { "EbsVolumeSizeInGb": 25 }
                    },
                },
            }),
        },
        Declared {
            domain: 0,
            // Carries no settings at all: every summary is then absent, which is what the
            // reader must report for a create that named none.
            name: "gamma",
            extra: json!({}),
        },
        Declared {
            domain: 1,
            name: "alpha-other-domain",
            extra: json!({ "SpaceSharingSettings": { "SharingType": "Shared" } }),
        },
    ]
}

/// Create a domain in `REGION` and answer the `DomainId` it minted, once the clock has moved
/// it `InService` — a domain admits a space only from that status.
async fn in_service_domain(emulator: &Emulator, name: &str) -> String {
    let created = emulator
        .ok("CreateDomain", &minimal_domain_with_auth(name, "IAM"))
        .await;
    let domain_id = str_field(&created, "DomainId");
    wait_for_domain_in_service(emulator, REGION, &domain_id).await;
    domain_id
}

/// The `ListSpaces` page `request` asks for, as the list of `(DomainId, SpaceName)` pairs it
/// reports, in the order it reported them, with the page's own `NextToken`.
async fn listed(
    emulator: &Emulator,
    case: &str,
    request: &Value,
) -> (Vec<(String, String)>, Option<String>) {
    let (status, page) = emulator.call("ListSpaces", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListSpaces, got {status} with {page}"
    );
    let spaces = page
        .get("Spaces")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected a Spaces list, got {page}"))
        .iter()
        .map(|space| {
            (
                str_field(space, "DomainId"),
                str_field(space, "SpaceName"),
            )
        })
        .collect();
    let next = page
        .get("NextToken")
        .and_then(Value::as_str)
        .map(str::to_string);
    (spaces, next)
}

/// RULE: `ListSpaces` reports the spaces the store holds, narrowed by the search members the
/// request names and ordered by the instant it asked for.
///
/// The rows span the search members (the domain, the name fragment, both and neither), both
/// sort keys and both sort orders. Every expected page is computed from what the creates
/// carried, in the order the creates were made, never read out of the answer being checked.
#[tokio::test]
async fn list_spaces_reports_the_spaces_each_search_member_narrows_to() {
    let emulator = Emulator::new();
    let domains = [
        in_service_domain(&emulator, "listing-domain-one").await,
        in_service_domain(&emulator, "listing-domain-two").await,
    ];

    // The creates are made in a fixed order, so the creation-time ordering the listing
    // reports is this order — the pair of each space is what every row below is computed
    // from.
    let mut created: Vec<(String, String)> = Vec::new();
    for space in declared() {
        let domain_id = domains[space.domain].clone();
        let mut request = json!({ "DomainId": domain_id, "SpaceName": space.name });
        for (member, value) in space.extra.as_object().expect("an object of members") {
            request[member] = value.clone();
        }
        let answered = emulator.ok("CreateSpace", &request).await;
        let arn = str_field(&answered, "SpaceArn");
        assert_eq!(
            arn,
            format!("arn:aws:sagemaker:{REGION}:{ACCOUNT}:space/{domain_id}/{}", space.name),
            "case create-{}: expected the modelled space ARN, got {arn:?}",
            space.name
        );
        created.push((domain_id, space.name.to_string()));
    }

    // (case, the request, the pairs the page must carry in that order)
    let everything: Vec<(String, String)> = created.clone();
    let first_domain: Vec<(String, String)> = created
        .iter()
        .filter(|(domain_id, _)| *domain_id == domains[0])
        .cloned()
        .collect();
    let named_alpha: Vec<(String, String)> = created
        .iter()
        .filter(|(_, name)| name.contains("alpha"))
        .cloned()
        .collect();
    let alpha_of_first_domain: Vec<(String, String)> = named_alpha
        .iter()
        .filter(|(domain_id, _)| *domain_id == domains[0])
        .cloned()
        .collect();
    let mut newest_first = everything.clone();
    newest_first.reverse();
    let rows: Vec<ListingRow> = vec![
        (
            "no search member narrows anything",
            json!({ "MaxResults": 100 }),
            everything.clone(),
        ),
        (
            "the domain narrows the page to its own spaces",
            json!({ "DomainIdEquals": domains[0].clone(), "MaxResults": 100 }),
            first_domain,
        ),
        (
            "the name fragment narrows the page to the names that carry it",
            json!({ "SpaceNameContains": "alpha", "MaxResults": 100 }),
            named_alpha,
        ),
        (
            "both members narrow together",
            json!({
                "DomainIdEquals": domains[0].clone(),
                "SpaceNameContains": "alpha",
                "MaxResults": 100,
            }),
            alpha_of_first_domain,
        ),
        (
            "a fragment no name carries narrows the page to nothing",
            json!({ "SpaceNameContains": "nosuchfragment", "MaxResults": 100 }),
            Vec::new(),
        ),
        (
            "the descending order reverses the page",
            json!({ "SortOrder": "Descending", "MaxResults": 100 }),
            newest_first,
        ),
        (
            // Nothing has changed any space since its create, so ordering by the instant each
            // last changed is the same order as ordering by its creation.
            "the other sort key orders by the instant each space last changed",
            json!({ "SortBy": "LastModifiedTime", "MaxResults": 100 }),
            everything,
        ),
    ];
    for (case, request, expected) in rows {
        let (page, next) = listed(&emulator, case, &request).await;
        assert_eq!(
            page, expected,
            "case {case:?}: expected the page {expected:?}, got {page:?}"
        );
        assert_eq!(
            next, None,
            "case {case:?}: a page carrying everything left must carry no token, got {next:?}"
        );
    }
}

/// RULE: `MaxResults` caps a page and the `NextToken` it answers resumes exactly where that
/// page stopped, so a paginating client walks every space exactly once.
#[tokio::test]
async fn list_spaces_pages_through_every_space_exactly_once() {
    let emulator = Emulator::new();
    let domain_id = in_service_domain(&emulator, "paging-domain").await;
    let names = ["page-one", "page-two", "page-three", "page-four"];
    for name in names {
        emulator
            .ok(
                "CreateSpace",
                &json!({ "DomainId": domain_id.clone(), "SpaceName": name }),
            )
            .await;
    }

    // One space per page, and the page size the model's own default (10) for the whole set.
    for page_size in [1, 2, 3] {
        let case = format!("pages of {page_size}");
        let mut walked: Vec<(String, String)> = Vec::new();
        let mut token: Option<String> = None;
        // The walk is bounded by the spaces that exist: one more page than the set can fill
        // is already a token that never clears.
        for _ in 0..=names.len() {
            let mut request = json!({
                "DomainIdEquals": domain_id.clone(),
                "MaxResults": page_size,
            });
            if let Some(token) = &token {
                request["NextToken"] = json!(token);
            }
            let (page, next) = listed(&emulator, &case, &request).await;
            assert!(
                page.len() <= page_size as usize,
                "case {case:?}: expected at most {page_size} per page, got {page:?}"
            );
            walked.extend(page);
            token = next;
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            token, None,
            "case {case:?}: the walk must end with no token left, got {token:?}"
        );
        let expected: Vec<(String, String)> = names
            .iter()
            .map(|name| (domain_id.clone(), (*name).to_string()))
            .collect();
        assert_eq!(
            walked, expected,
            "case {case:?}: expected every space exactly once in creation order, got {walked:?}"
        );
    }

    // A token naming a space the listing no longer reports is refused rather than silently
    // starting over: the page it resumed from is gone.
    let (_, token) = listed(
        &emulator,
        "a token to be staled",
        &json!({ "DomainIdEquals": domain_id.clone(), "MaxResults": 1 }),
    )
    .await;
    let token = token.expect("a first page of four leaves a token");
    emulator
        .ok(
            "DeleteSpace",
            &json!({ "DomainId": domain_id.clone(), "SpaceName": names[0] }),
        )
        .await;
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, refused) = emulator
            .call(
                "ListSpaces",
                &json!({
                    "DomainIdEquals": domain_id.clone(),
                    "MaxResults": 1,
                    "NextToken": token.clone(),
                }),
            )
            .await;
        if status == 400 {
            assert_eq!(
                error_code(&refused),
                "ValidationException",
                "case \"a stale token\": expected the modelled refusal, got {refused}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case \"a stale token\": expected the token to stale once the space it names is \
             gone, last read {status} {refused}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: each entry of a page summarises the settings the space's own create stored — the
/// members the summary shapes carry — and reports none for a create that carried none.
#[tokio::test]
async fn a_listed_space_summarises_the_settings_its_create_stored() {
    let emulator = Emulator::new();
    let domain_id = in_service_domain(&emulator, "summary-domain").await;
    emulator
        .ok(
            "CreateSpace",
            &json!({
                "DomainId": domain_id.clone(),
                "SpaceName": "summarised",
                "SpaceDisplayName": "The summarised space",
                "SpaceSettings": {
                    "AppType": "JupyterLab",
                    "RemoteAccess": "ENABLED",
                    "SpaceStorageSettings": {
                        "EbsStorageSettings": { "EbsVolumeSizeInGb": 30 }
                    },
                },
                "SpaceSharingSettings": { "SharingType": "Private" },
            }),
        )
        .await;
    emulator
        .ok(
            "CreateSpace",
            &json!({ "DomainId": domain_id.clone(), "SpaceName": "unsummarised" }),
        )
        .await;

    let (status, page) = emulator
        .call(
            "ListSpaces",
            &json!({ "DomainIdEquals": domain_id.clone(), "MaxResults": 100 }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"the summaries\": expected HTTP 200 from ListSpaces, got {status} with {page}"
    );
    let spaces = page["Spaces"]
        .as_array()
        .unwrap_or_else(|| panic!("case \"the summaries\": expected a Spaces list, got {page}"));
    let entry = |name: &str| -> Value {
        spaces
            .iter()
            .find(|space| space.get("SpaceName").and_then(Value::as_str) == Some(name))
            .unwrap_or_else(|| panic!("case {name:?}: expected the space in {page}"))
            .clone()
    };

    let summarised = entry("summarised");
    assert_eq!(
        summarised.get("SpaceDisplayName").and_then(Value::as_str),
        Some("The summarised space"),
        "case \"summarised\": expected the declared display name, got {summarised}"
    );
    assert_eq!(
        summarised.get("SpaceSettingsSummary"),
        Some(&json!({
            "AppType": "JupyterLab",
            "RemoteAccess": "ENABLED",
            "SpaceStorageSettings": { "EbsStorageSettings": { "EbsVolumeSizeInGb": 30 } },
        })),
        "case \"summarised\": expected the settings summary the create stored, got {summarised}"
    );
    assert_eq!(
        summarised.get("SpaceSharingSettingsSummary"),
        Some(&json!({ "SharingType": "Private" })),
        "case \"summarised\": expected the sharing summary, got {summarised}"
    );
    // The space is created `Pending` and the clock owes it `InService`, so either is the
    // status the page may report — and no other.
    let status_reported = str_field(&summarised, "Status");
    assert!(
        [SPACE_STATUS_PENDING, SPACE_STATUS_IN_SERVICE].contains(&status_reported.as_str()),
        "case \"summarised\": expected the created or the settled status, got \
         {status_reported:?}"
    );

    let unsummarised = entry("unsummarised");
    for absent in [
        "SpaceSettingsSummary",
        "SpaceSharingSettingsSummary",
        "OwnershipSettingsSummary",
        "SpaceDisplayName",
    ] {
        assert_eq!(
            unsummarised.get(absent),
            None,
            "case \"unsummarised\": expected no {absent} for a create that carried none, got \
             {unsummarised}"
        );
    }
}

/// RULE: a space OWNED by a user profile reports that owner in its ownership summary, which
/// is the one member that summary shape carries.
#[tokio::test]
async fn a_listed_space_summarises_the_profile_that_owns_it() {
    let emulator = Emulator::new();
    let domain_id = in_service_domain(&emulator, "ownership-domain").await;
    let owner = "space-owner";
    emulator
        .ok(
            "CreateUserProfile",
            &json!({ "DomainId": domain_id.clone(), "UserProfileName": owner }),
        )
        .await;
    emulator
        .ok(
            "CreateSpace",
            &json!({
                "DomainId": domain_id.clone(),
                "SpaceName": "owned",
                "OwnershipSettings": { "OwnerUserProfileName": owner },
                "SpaceSharingSettings": { "SharingType": "Private" },
            }),
        )
        .await;
    let page = emulator
        .ok(
            "ListSpaces",
            &json!({ "DomainIdEquals": domain_id.clone(), "MaxResults": 100 }),
        )
        .await;
    let owned = page["Spaces"]
        .as_array()
        .and_then(|spaces| spaces.first())
        .unwrap_or_else(|| panic!("case \"owned\": expected the one space, got {page}"));
    assert_eq!(
        owned.get("OwnershipSettingsSummary"),
        Some(&json!({ "OwnerUserProfileName": owner })),
        "case \"owned\": expected the owner the create named, got {owned}"
    );
}

/// RULE: the tag set a resource's own create stored is the one the tag operations act on, so a
/// domain's `CreateDomain` tags are what `ListTags` reports, what `AddTags` adds to and what
/// `DeleteTags` removes from; an ARN of that family this account holds no domain under is
/// refused in the domain family's own words.
#[tokio::test]
async fn a_domains_tags_are_the_set_the_tag_operations_act_on() {
    let emulator = Emulator::new();
    let mut request = minimal_domain_with_auth("tagged-domain", "IAM");
    request["Tags"] = json!([
        { "Key": "team", "Value": "studio" },
        { "Key": "stage", "Value": "test" },
    ]);
    let created = emulator.ok("CreateDomain", &request).await;
    let domain_id = str_field(&created, "DomainId");
    let domain_arn = str_field(&created, "DomainArn");
    assert_eq!(
        domain_arn,
        format!("arn:aws:sagemaker:{REGION}:{ACCOUNT}:domain/{domain_id}"),
        "case \"the tagged domain\": expected the modelled domain ARN, got {domain_arn:?}"
    );

    // (case, the keys the tag set must then carry, the request that got it there)
    let listed_keys = |answered: &Value| -> Vec<String> {
        let mut keys: Vec<String> = answered
            .get("Tags")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("expected a Tags list, got {answered}"))
            .iter()
            .map(|tag| str_field(tag, "Key"))
            .collect();
        keys.sort();
        keys
    };

    let reported = emulator
        .ok("ListTags", &json!({ "ResourceArn": domain_arn.clone() }))
        .await;
    assert_eq!(
        listed_keys(&reported),
        vec!["stage".to_string(), "team".to_string()],
        "case \"the create's own tags\": expected the keys the create stored, got {reported}"
    );
    assert_eq!(
        reported["Tags"]
            .as_array()
            .and_then(|tags| tags.iter().find(|tag| str_field(tag, "Key") == "team"))
            .map(|tag| str_field(tag, "Value")),
        Some("studio".to_string()),
        "case \"the create's own tags\": expected the value the create stored, got {reported}"
    );

    emulator
        .ok(
            "AddTags",
            &json!({
                "ResourceArn": domain_arn.clone(),
                "Tags": [{ "Key": "owner", "Value": "platform" }],
            }),
        )
        .await;
    let reported = emulator
        .ok("ListTags", &json!({ "ResourceArn": domain_arn.clone() }))
        .await;
    assert_eq!(
        listed_keys(&reported),
        vec!["owner".to_string(), "stage".to_string(), "team".to_string()],
        "case \"a tag added\": expected the added key beside the create's own, got {reported}"
    );

    emulator
        .ok(
            "DeleteTags",
            &json!({ "ResourceArn": domain_arn.clone(), "TagKeys": ["team"] }),
        )
        .await;
    let reported = emulator
        .ok("ListTags", &json!({ "ResourceArn": domain_arn.clone() }))
        .await;
    assert_eq!(
        listed_keys(&reported),
        vec!["owner".to_string(), "stage".to_string()],
        "case \"a tag removed\": expected the key to be gone, got {reported}"
    );

    // An ARN of the domain family this account holds nothing under: refused with the sentence
    // the domain's own readers word that fact in, naming the ARN the request resolved to.
    let absent = format!("arn:aws:sagemaker:{REGION}:{ACCOUNT}:domain/d-000000000000");
    let (status, refused) = emulator
        .call("ListTags", &json!({ "ResourceArn": absent.clone() }))
        .await;
    assert_eq!(
        status, 400,
        "case \"an absent domain\": expected the tag read to be refused, got {status} with \
         {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "ValidationException",
        "case \"an absent domain\": expected the modelled refusal, got {refused}"
    );
    assert_eq!(
        crate::inference_deployment_lifecycle::message(&refused),
        format!("Domain [{absent}] does not exist."),
        "case \"an absent domain\": expected the domain family's own sentence, got {refused}"
    );
}
