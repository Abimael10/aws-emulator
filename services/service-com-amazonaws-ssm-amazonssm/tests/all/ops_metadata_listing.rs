// SPDX-License-Identifier: CC0-1.0

//! What `ListOpsMetadata` reports of the Application Manager objects this account holds.
//!
//! RULE: a listing reports the objects the ENDPOINT's Region holds — each member read out of
//! what its create persisted, in ascending `OpsMetadataArn` order — narrowed to the ones
//! meeting EVERY filter the request carries, and bounded by the page it asked for, with the
//! ARN of the object left over past the page handed back as the `NextToken` the next page
//! resumes at. An argument the listing cannot act on is refused with the modelled
//! `OpsMetadataInvalidArgumentException`: a filter key it does not search on, or a token it
//! did not issue (`smithy_query(query="operation", operation="ListOpsMetadata")`).
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`call`], [`call_in_region`], [`error_code`], [`error_message`],
//! [`router`]) dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>` through the
//! multi-service router — and every object a listing reports was created through that same
//! door, so a listing that is not computed from persisted state cannot pass.
//!
//! The rows span what the rule generalises over: how many objects are held, which Region
//! holds them, which filter key and values narrow them, how many values one filter carries,
//! how large a page is asked for and where that page sits against the published bounds, and
//! where a token came from. What the single-object reads and the writes do is
//! `all/ops_metadata.rs`'s subject.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, error_message, router, ACCOUNT, REGION};

/// The applications a row names, in the order a listing must report them.
type Applications<'a> = &'a [&'a str];

/// The `(key, value)` pairs of the `MetadataMap` a create carries.
type Entries<'a> = &'a [(&'a str, &'a str)];

/// A second Region, so a case can prove a listing reports the objects of the ENDPOINT's
/// Region rather than of every Region at once.
const OTHER_REGION: &str = "eu-central-1";

/// The filter keys a listing searches on, written out here independently of the service, in
/// the order the refusal of any other key lists them back to the caller
/// (the recorded sentence `ListOpsMetadata` answers an unsearched key with).
const FILTER_KEYS: &[&str] = &["ResourceId", "provisioned-by", "application-type"];

/// The published floor of `ListOpsMetadataMaxResults`
/// (`smithy_query(query="shape", shape="ListOpsMetadataMaxResults")`: `range 1..50`).
const MIN_PAGE_SIZE: i64 = 1;

/// The published ceiling of the same member.
const MAX_PAGE_SIZE: i64 = 50;

/// The ARN an object registered for the application `name` in `region` is reached by, written
/// out here independently of the service, as `all/ops_metadata.rs` writes it.
fn ops_metadata_arn(region: &str, name: &str) -> String {
    format!("arn:aws:ssm:{region}:{ACCOUNT}:opsmetadata/aws/ssm/{name}/opsmetadata")
}

/// The ARNs of `applications` in `region` — what a listing of them must report.
fn arns_in_region(region: &str, applications: Applications) -> Vec<String> {
    applications
        .iter()
        .map(|application| ops_metadata_arn(region, application))
        .collect()
}

/// A `MetadataMap` body carrying one entry per `(key, value)` pair.
fn metadata(entries: Entries) -> Value {
    let mut map = serde_json::Map::new();
    for (key, value) in entries {
        map.insert((*key).to_string(), json!({ "Value": value }));
    }
    Value::Object(map)
}

/// `CreateOpsMetadata` for the application `name` at `region`'s endpoint, carrying `entries`,
/// asserting it was served and returning the ARN it minted.
async fn create_in_region(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    entries: Entries<'_>,
) -> String {
    let mut request = json!({ "ResourceId": name });
    if !entries.is_empty() {
        request["Metadata"] = metadata(entries);
    }
    let (status, created) = call_in_region(router, region, "CreateOpsMetadata", &request).await;
    assert_eq!(
        status, 200,
        "case {name:?}: the create at {region} must be served, got {status} with {created}"
    );
    created["OpsMetadataArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: a create reports an OpsMetadataArn, got {created}"))
        .to_string()
}

/// `CreateOpsMetadata` at [`REGION`]'s endpoint.
async fn create(router: &MultiServiceRouter, name: &str, entries: Entries<'_>) -> String {
    create_in_region(router, REGION, name, entries).await
}

/// `ListOpsMetadata` with `body` at `region`'s endpoint, returning `(status, body)`.
async fn list_in_region(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    call_in_region(router, region, "ListOpsMetadata", body).await
}

/// The `OpsMetadataArn` of each object a listing body reported, in order.
fn arns_of(listing: &Value) -> Vec<String> {
    listing["OpsMetadataList"]
        .as_array()
        .map(|objects| {
            objects
                .iter()
                .map(|object| {
                    object["OpsMetadataArn"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `ListOpsMetadata` with `body` at [`REGION`], asserting it was served and returning the
/// ARNs it reported, in order.
async fn listed_arns(router: &MultiServiceRouter, case: &str, body: &Value) -> Vec<String> {
    let (status, listing) = list_in_region(router, REGION, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the listing {body} must be served, got {status} with {listing}"
    );
    assert!(
        listing["OpsMetadataList"].is_array(),
        "case {case:?}: a listing reports an OpsMetadataList, got {listing}"
    );
    arns_of(&listing)
}

/// RULE: every member of a listed object is read out of what its create persisted, and an
/// object nothing created is reported by no listing.
#[tokio::test]
async fn a_listing_reports_what_the_creates_persisted() {
    // A second, untouched emulator beside the one the creates write to: an account that holds
    // no object must be reported as holding none. Built first, since the binding below
    // shadows the harness's constructor.
    let empty_account = router();
    let router = router();
    // (the application, the resource id its create names, its metadata entries)
    let rows: [(&str, String, Entries); 3] = [
        ("MyApplication", "MyApplication".to_string(), &[]),
        (
            "Payments",
            format!("arn:aws:resource-groups:{REGION}:{ACCOUNT}:group/Payments"),
            &[("provisioned-by", "AWS CloudFormation")],
        ),
        (
            "web",
            "web".to_string(),
            &[("application-type", "Custom"), ("owner", "retail")],
        ),
    ];
    for (_, resource_id, entries) in &rows {
        create(&router, resource_id, entries).await;
    }

    let (status, listing) = list_in_region(&router, REGION, &json!({})).await;
    assert_eq!(
        status, 200,
        "the listing must be served, got {status} with {listing}"
    );
    let reported = listing["OpsMetadataList"]
        .as_array()
        .unwrap_or_else(|| panic!("a listing reports an OpsMetadataList, got {listing}"))
        .clone();
    assert_eq!(
        reported.len(),
        rows.len(),
        "expected one object per create, got {listing}"
    );

    for (application, resource_id, _) in &rows {
        let arn = ops_metadata_arn(REGION, application);
        let object = reported
            .iter()
            .find(|object| object["OpsMetadataArn"].as_str() == Some(arn.as_str()))
            .unwrap_or_else(|| panic!("case {application:?}: the listing reports {arn}, got {listing}"));
        assert_eq!(
            object["ResourceId"].as_str(),
            Some(resource_id.as_str()),
            "case {application:?}: the summary carries the created resource id, got {object}"
        );
        let created = object["CreationDate"].as_f64().unwrap_or_else(|| {
            panic!("case {application:?}: a summary reports a CreationDate, got {object}")
        });
        let modified = object["LastModifiedDate"].as_f64().unwrap_or_else(|| {
            panic!("case {application:?}: a summary reports a LastModifiedDate, got {object}")
        });
        assert!(
            modified >= created,
            "case {application:?}: expected LastModifiedDate >= CreationDate, got {modified} \
             and {created}"
        );
        assert!(
            !object["LastModifiedUser"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "case {application:?}: expected the principal that wrote the object, got {object}"
        );
    }

    // An account that holds none is reported as holding none, rather than as holding
    // something nobody created.
    let empty = listed_arns(&empty_account, "an account holding no object", &json!({})).await;
    assert_eq!(
        empty,
        Vec::<String>::new(),
        "case \"an account holding no object\": expected the empty set, got {empty:?}"
    );
}

/// RULE: a listing reports the objects the endpoint's Region holds, in ascending ARN order,
/// and nothing of another Region.
#[tokio::test]
async fn a_listing_reports_the_objects_of_its_own_region() {
    let router = router();
    let here = ["Alpha", "Beta", "Gamma"];
    for application in here {
        create(&router, application, &[]).await;
    }
    create_in_region(&router, OTHER_REGION, "Delta", &[]).await;

    let reported = listed_arns(&router, "every object of the Region", &json!({})).await;
    assert_eq!(
        reported,
        arns_in_region(REGION, &here),
        "case \"every object of the Region\": expected the Region's own objects in ARN order, \
         got {reported:?}"
    );

    let (status, elsewhere) = list_in_region(&router, OTHER_REGION, &json!({})).await;
    assert_eq!(
        status, 200,
        "case \"the other Region\": the listing must be served, got {status} with {elsewhere}"
    );
    assert_eq!(
        arns_of(&elsewhere),
        arns_in_region(OTHER_REGION, &["Delta"]),
        "case \"the other Region\": expected only its own object, got {elsewhere}"
    );
}

/// RULE: a listing is narrowed to the objects meeting EVERY filter it carries, each value of
/// one filter being an alternative.
#[tokio::test]
async fn a_listing_is_narrowed_by_every_filter_it_carries() {
    let router = router();
    // The Application Manager properties the two non-`ResourceId` keys search on are carried
    // by an object's metadata entries, so `Alpha` and `Beta` were provisioned by
    // CloudFormation and `Gamma` by nothing that writes them.
    create(
        &router,
        "Alpha",
        &[
            ("provisioned-by", "AWS CloudFormation"),
            ("application-type", "CloudFormation"),
        ],
    )
    .await;
    create(
        &router,
        "Beta",
        &[
            ("provisioned-by", "AWS CloudFormation"),
            ("application-type", "Custom"),
        ],
    )
    .await;
    create(&router, "Gamma", &[("owner", "retail")]).await;

    // (case, Filters, the applications the listing must report)
    let rows: &[(&str, Value, Applications)] = &[
        (
            "one resource id",
            json!([{ "Key": "ResourceId", "Values": ["Beta"] }]),
            &["Beta"],
        ),
        (
            "two values of one key",
            json!([{ "Key": "ResourceId", "Values": ["Alpha", "Gamma"] }]),
            &["Alpha", "Gamma"],
        ),
        (
            "a resource id nothing holds",
            json!([{ "Key": "ResourceId", "Values": ["Omega"] }]),
            &[],
        ),
        (
            "two filters that cannot both hold",
            json!([
                { "Key": "ResourceId", "Values": ["Alpha"] },
                { "Key": "ResourceId", "Values": ["Beta"] },
            ]),
            &[],
        ),
        (
            "a filter naming every object",
            json!([{ "Key": "ResourceId", "Values": ["Alpha", "Beta", "Gamma"] }]),
            &["Alpha", "Beta", "Gamma"],
        ),
        (
            "what provisioned the application",
            json!([{ "Key": "provisioned-by", "Values": ["AWS CloudFormation"] }]),
            &["Alpha", "Beta"],
        ),
        (
            "a provisioner nothing names",
            json!([{ "Key": "provisioned-by", "Values": ["AWS Service Catalog"] }]),
            &[],
        ),
        (
            "the application type",
            json!([{ "Key": "application-type", "Values": ["Custom"] }]),
            &["Beta"],
        ),
        (
            "two application types at once",
            json!([{ "Key": "application-type", "Values": ["Custom", "CloudFormation"] }]),
            &["Alpha", "Beta"],
        ),
        (
            "a type and a provisioner that both hold",
            json!([
                { "Key": "application-type", "Values": ["CloudFormation"] },
                { "Key": "provisioned-by", "Values": ["AWS CloudFormation"] },
            ]),
            &["Alpha"],
        ),
        (
            "a type and a resource id that cannot both hold",
            json!([
                { "Key": "application-type", "Values": ["Custom"] },
                { "Key": "ResourceId", "Values": ["Alpha"] },
            ]),
            &[],
        ),
    ];
    for (case, filters, wanted) in rows {
        let reported = listed_arns(&router, case, &json!({ "Filters": filters })).await;
        assert_eq!(
            reported,
            arns_in_region(REGION, wanted),
            "case {case:?}: expected {wanted:?}, got {reported:?}"
        );
    }
}

/// RULE: a filter key a listing does not search on names no property of an OpsMetadata
/// object, so the listing is refused as the invalid argument it is rather than answered.
#[tokio::test]
async fn a_filter_key_the_listing_does_not_search_on_is_refused() {
    let router = router();
    create(&router, "Alpha", &[]).await;
    // Keys outside `OPS_METADATA_FILTER_KEYS`: near misses in case and spelling of the keys a
    // listing does search on, members of the object that are not filter keys, and a key
    // another listing of this service accepts.
    for key in [
        "resourceId",
        "RESOURCEID",
        "ProvisionedBy",
        "provisioned_by",
        "ApplicationType",
        "OpsMetadataArn",
        "LastModifiedUser",
        "Name",
        "NotAKey",
    ] {
        let (status, body) = list_in_region(
            &router,
            REGION,
            &json!({ "Filters": [{ "Key": key, "Values": ["anything"] }] }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {key:?}: the listing must be refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "OpsMetadataInvalidArgumentException",
            "case {key:?}: expected the invalid-argument refusal, got {body}"
        );
        let expected = format!(
            "The following OpsMetadataFilter key is not valid: {key}. Valid filter keys \
             include: [ResourceId, provisioned-by, application-type]."
        );
        let message = error_message(&body);
        assert_eq!(
            message, expected,
            "case {key:?}: expected {expected:?}, got {message:?}"
        );
    }
    // The keys it DOES search on are answered rather than refused, however little they narrow
    // the listing to.
    for key in FILTER_KEYS {
        let (status, body) = list_in_region(
            &router,
            REGION,
            &json!({ "Filters": [{ "Key": key, "Values": ["anything"] }] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {key:?}: a key a listing searches on must be answered, got {status} with \
             {body}"
        );
    }
}

/// RULE: a page is bounded by `MaxResults`, the object left over past it is handed back as
/// the `NextToken` the next page resumes at, and a page size outside the published range is
/// refused before any listing runs.
#[tokio::test]
async fn a_page_is_bounded_and_the_leftover_is_resumed_by_the_token() {
    let router = router();
    let applications = ["Alpha", "Beta", "Gamma", "Delta", "Epsilon"];
    for application in applications {
        create(&router, application, &[]).await;
    }
    let mut every = arns_in_region(REGION, &applications);
    every.sort();

    for page_size in [MIN_PAGE_SIZE, 2, 4, 5, MAX_PAGE_SIZE] {
        let case = format!("MaxResults {page_size}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut request = json!({ "MaxResults": page_size });
            if let Some(token) = token.as_deref() {
                request["NextToken"] = json!(token);
            }
            let (status, page) = list_in_region(&router, REGION, &request).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the page must be served, got {status} with {page}"
            );
            let reported = arns_of(&page);
            assert!(
                reported.len() <= page_size as usize,
                "case {case:?}: a page holds at most what was asked for, got {reported:?}"
            );
            seen.extend(reported);
            pages += 1;
            assert!(
                pages <= applications.len() + 1,
                "case {case:?}: the walk must terminate, got {page}"
            );
            match page["NextToken"].as_str() {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            seen, every,
            "case {case:?}: expected every object exactly once, got {seen:?}"
        );
    }

    // One past each published bound: outside the modelled range the boundary refuses the
    // request before any listing runs.
    for asked in [MIN_PAGE_SIZE - 1, MAX_PAGE_SIZE + 1] {
        let (status, body) = list_in_region(&router, REGION, &json!({ "MaxResults": asked })).await;
        assert_eq!(
            status, 400,
            "case \"MaxResults {asked}\": must be refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case \"MaxResults {asked}\": expected the boundary's refusal, got {body}"
        );
    }
}

/// RULE: a `NextToken` this listing did not issue resumes no page of this result, so it is
/// refused rather than answered with an arbitrary page — and the token a page DID hand out
/// resumes exactly where that page stopped.
#[tokio::test]
async fn a_token_this_listing_did_not_issue_is_refused() {
    let router = router();
    create(&router, "Alpha", &[]).await;
    create(&router, "Beta", &[]).await;

    for token in [
        ops_metadata_arn(REGION, "Omega"),
        ops_metadata_arn(OTHER_REGION, "Alpha"),
        "not-a-token".to_string(),
        "0".to_string(),
    ] {
        let (status, body) = list_in_region(&router, REGION, &json!({ "NextToken": token })).await;
        assert_eq!(
            status, 400,
            "case {token:?}: a token this listing did not issue must be refused, got {status} \
             with {body}"
        );
        assert_eq!(
            error_code(&body),
            "OpsMetadataInvalidArgumentException",
            "case {token:?}: expected the invalid-argument refusal, got {body}"
        );
    }

    let (status, first) = list_in_region(&router, REGION, &json!({ "MaxResults": 1 })).await;
    assert_eq!(
        status, 200,
        "case \"the first page\": must be served, got {status} with {first}"
    );
    assert_eq!(
        arns_of(&first),
        arns_in_region(REGION, &["Alpha"]),
        "case \"the first page\": expected the first object in ARN order, got {first}"
    );
    let issued = first["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"the first page\": expected a NextToken, got {first}"));
    let resumed = listed_arns(&router, "the issued token", &json!({ "NextToken": issued })).await;
    assert_eq!(
        resumed,
        arns_in_region(REGION, &["Beta"]),
        "case \"the issued token\": expected the object the token names, got {resumed:?}"
    );
}

/// RULE: a listing reports nothing a delete dropped, so what it reports follows the writes.
#[tokio::test]
async fn a_listing_reports_nothing_a_delete_dropped() {
    let router = router();
    create(&router, "Alpha", &[]).await;
    let kept = create(&router, "Beta", &[]).await;
    let dropped = ops_metadata_arn(REGION, "Alpha");

    let (status, deleted) = call_in_region(
        &router,
        REGION,
        "DeleteOpsMetadata",
        &json!({ "OpsMetadataArn": dropped }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the delete must be served, got {status} with {deleted}"
    );

    let reported = listed_arns(&router, "after the delete", &json!({})).await;
    assert_eq!(
        reported,
        vec![kept],
        "case \"after the delete\": expected only the object left, got {reported:?}"
    );
}
