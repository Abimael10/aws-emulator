// SPDX-License-Identifier: CC0-1.0

//! A Systems Manager resource tagged through SSM's OWN tag plane is a row of the Resource
//! Groups Tagging index.
//!
//! The rule under test: **a tag the service that OWNS a resource holds for it — here the
//! one tag set an SSM parameter, document or patch baseline carries, seeded by its
//! create's `Tags` or written by `AddTagsToResource` — is what the tagging index reports
//! for that resource in the Region the resource lives in, keyed by the ARN that service
//! mints for it, so `GetResources` selects the resource by its `TagFilters` (AND-ed across
//! filters, each filter's `Values` OR-ed, a filter with no values matching the key with
//! any value), optionally narrowed by `ResourceTypeFilters` in the
//! `service[:resourceType]` spelling that ARN carries — and `RemoveTagsFromResource`
//! withdraws exactly that resource from the index.** A resource carrying no tag is in no
//! answer ("GetResources does not return untagged resources", that operation's
//! documentation), and a resource of another Region is in no answer here ("Returns all the
//! tagged or previously tagged resources that are located in the specified Amazon Web
//! Services Region for the account").
//!
//! Two crates have to agree for that to hold — Systems Manager owns and stores the
//! resource's tag set, the tagging API projects it — so the suite lives here, on the
//! assembled router with both services mounted on ONE engine, which is how the emulator
//! runs.
//!
//! The rows span what the rule generalises over: the resource TYPE (a parameter, a
//! document, a patch baseline), how the tag got onto the resource (a create's `Tags`, a
//! later `AddTagsToResource`), the id form a tag write addresses a parameter by (a flat
//! name, a hierarchical one), how many keys the resource ends up with, an empty tag value,
//! a resource nothing ever tagged, and a resource of another Region; and the queries span
//! the filter forms — key only, key and value, several values, several filters, the bare
//! and the qualified resource type, a type nothing answers to, `ResourceARNList`, and no
//! filter at all. The parameter and the two documents the issue cites are rows.
//!
//! Every read of the index is taken in a poll loop that looks again until the condition
//! holds: the suite waits for the CONDITION, never for a fixed time.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_resourcegroupstaggingapi_resourcegroupstaggingapi_20170126 as tagging_api;
use service_com_amazonaws_ssm_amazonssm as owner_ssm;

/// The Region the estate is created in, and the Region the index is queried in.
const REGION: &str = "us-east-1";

/// A second Region, to pin the Region scope of the answer: a parameter here is reported by
/// this Region's endpoint and by no other.
const OTHER_REGION: &str = "us-west-2";

/// The account segment every ARN this emulator mints carries.
const ACCOUNT_ID: &str = engine::EMULATOR_ACCOUNT_ID;

/// The SigV4 signing names the router routes a request to a service by.
const TAGGING: &str = "tagging";
const SSM: &str = "ssm";

/// The `service:resourceType` keys the ARNs of this service's resources carry, and the bare
/// service token that selects every resource of the service.
const PARAMETER_TYPE: &str = "ssm:parameter";
const DOCUMENT_TYPE: &str = "ssm:document";
const PATCH_BASELINE_TYPE: &str = "ssm:patchbaseline";
const BARE_TYPE: &str = "ssm";

/// How long a poll loop keeps looking before it gives up. Generous on purpose: it bounds a
/// condition owed at most one short span, so a loaded or slow host still settles well
/// inside it.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop waits between looks — short relative to the span it waits out, so
/// the loop observes the change rather than the deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A minimal `Command` document, so a document exists to tag.
const DOCUMENT_CONTENT: &str = r#"{
  "schemaVersion": "2.2",
  "description": "A document the tagging index has to report",
  "mainSteps": [
    {
      "action": "aws:runShellScript",
      "name": "runShell",
      "inputs": { "runCommand": ["echo hi"] }
    }
  ]
}"#;

/// The router carrying Systems Manager and the tagging API over ONE engine, the
/// arrangement `core/router` builds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises handler behaviour, not SigV4: the `Authorization` header below
    // is present because its credential scope is what routes a request to a signing name
    // and names the Region, not to be verified.
    router.set_request_authentication(false);
    router.register_service(owner_ssm::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(tagging_api::EmulatedService::new(engine));
    router
}

/// Call one operation of one service, signed for `region`, and return its HTTP status and
/// parsed body.
async fn call(
    router: &MultiServiceRouter,
    region: &str,
    signing_name: &str,
    target: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", target)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=not-verified"
            ),
        )
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "{target} answered a body that is not JSON ({error}): {}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    (status, parsed)
}

/// One tagging-API operation in `region`, which must answer 200.
async fn tagging_ok(router: &MultiServiceRouter, region: &str, operation: &str, body: Value) -> Value {
    let (status, parsed) = call(
        router,
        region,
        TAGGING,
        &format!("ResourceGroupsTaggingAPI_20170126.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "{operation} in {region} must answer 200, got {status} with body {parsed}"
    );
    parsed
}

/// One Systems Manager operation in `region`, which must answer 200.
async fn ssm_ok(router: &MultiServiceRouter, region: &str, operation: &str, body: Value) -> Value {
    let (status, parsed) =
        call(router, region, SSM, &format!("AmazonSSM.{operation}"), body).await;
    assert_eq!(
        status, 200,
        "{operation} in {region} must answer 200, got {status} with body {parsed}"
    );
    parsed
}

/// `(Key, Value)` records as the modelled SSM `TagList` carries them.
fn tag_list(tags: &[(&str, &str)]) -> Value {
    Value::Array(
        tags.iter()
            .map(|(key, value)| json!({ "Key": key, "Value": value }))
            .collect(),
    )
}

/// A declared tag set as the map the assertions compare against.
fn tag_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// The `(ARN -> tags)` map one `GetResources` page reports.
fn page_estate(body: &Value) -> BTreeMap<String, BTreeMap<String, String>> {
    body["ResourceTagMappingList"]
        .as_array()
        .unwrap_or_else(|| panic!("a GetResources body with no ResourceTagMappingList: {body}"))
        .iter()
        .map(|mapping| {
            let arn = mapping["ResourceARN"]
                .as_str()
                .unwrap_or_else(|| panic!("a mapping with no ResourceARN: {mapping}"))
                .to_string();
            let tags = mapping["Tags"]
                .as_array()
                .unwrap_or_else(|| panic!("a mapping with no Tags: {mapping}"))
                .iter()
                .map(|tag| {
                    (
                        tag["Key"]
                            .as_str()
                            .unwrap_or_else(|| panic!("a tag with no Key: {tag}"))
                            .to_string(),
                        tag["Value"]
                            .as_str()
                            .unwrap_or_else(|| panic!("a tag with no Value: {tag}"))
                            .to_string(),
                    )
                })
                .collect();
            (arn, tags)
        })
        .collect()
}

/// The ARNs one `GetResources` request selects in `region`, sorted, with the answer for a
/// message.
async fn selected(router: &MultiServiceRouter, region: &str, request: Value) -> (Vec<String>, Value) {
    let body = tagging_ok(router, region, "GetResources", request).await;
    let mut arns: Vec<String> = page_estate(&body).into_keys().collect();
    arns.sort();
    (arns, body)
}

/// The tag map SSM's own `ListTagsForResource` reports for one resource.
async fn listed_tags(
    router: &MultiServiceRouter,
    region: &str,
    resource_type: &str,
    resource_id: &str,
) -> BTreeMap<String, String> {
    let body = ssm_ok(
        router,
        region,
        "ListTagsForResource",
        json!({ "ResourceType": resource_type, "ResourceId": resource_id }),
    )
    .await;
    body["TagList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|tag| {
            (
                tag["Key"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a tag with no Key: {tag}"))
                    .to_string(),
                tag["Value"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a tag with no Value: {tag}"))
                    .to_string(),
            )
        })
        .collect()
}

/// Look again until `settled` answers `Ok`, or [`POLL_DEADLINE`] passes.
///
/// The loop waits for the CONDITION and reports the last answer it saw when it runs out of
/// patience, which is what names the failure.
async fn poll_until<T, F, Fut>(case: &str, what: &str, mut settled: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let started = Instant::now();
    loop {
        let last = match settled().await {
            Ok(value) => return value,
            Err(seen) => seen,
        };
        if started.elapsed() >= POLL_DEADLINE {
            panic!("case {case:?}: {what} never settled; the last answer was {last}");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Which SSM resource one row of the estate is, which is what its create takes and what the
/// `ResourceType` of a tag write addressing it names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A Parameter Store parameter, created by `PutParameter`.
    Parameter,
    /// An SSM document, created by `CreateDocument`.
    Document,
    /// A Patch Manager baseline, created by `CreatePatchBaseline`.
    PatchBaseline,
}

impl Kind {
    /// The `ResourceTypeForTagging` value a tag write addressing this kind names
    /// (`smithy_query(query="shape", shape="ResourceTypeForTagging")`).
    fn tagging_type(self) -> &'static str {
        match self {
            Kind::Parameter => "Parameter",
            Kind::Document => "Document",
            Kind::PatchBaseline => "PatchBaseline",
        }
    }
}

/// One resource of the estate: how its tags got onto it, and the set it carries once both
/// writes have landed.
struct Row {
    case: &'static str,
    kind: Kind,
    /// The name its create gives it — for a patch baseline, also the name the create takes,
    /// whose id the create mints.
    name: &'static str,
    /// The `Tags` its create declares.
    seeded: &'static [(&'static str, &'static str)],
    /// The pairs a later `AddTagsToResource` writes.
    written: &'static [(&'static str, &'static str)],
    /// The set the resource carries afterwards, by the rule `AddTagsToResource` states
    /// ("Adds or overwrites one or more tags"), computed from the declarations rather than
    /// read back out of the answer.
    settled: &'static [(&'static str, &'static str)],
}

/// The estate of [`REGION`]: the parameter the issue cites, a hierarchical parameter tagged
/// only by `AddTagsToResource`, a parameter whose create's key a later write overwrites
/// beside one it empties (`TagValueString` admits an empty value, so an empty value is a
/// value and not an absence), a parameter nothing ever tagged, the two documents the issue
/// cites, and a patch baseline — so the rule is read on three resource types and both
/// tagging paths.
const ROWS: &[Row] = &[
    Row {
        case: "a parameter tagged by its own create",
        kind: Kind::Parameter,
        name: "native-create-one",
        seeded: &[("Project", "phoenix")],
        written: &[],
        settled: &[("Project", "phoenix")],
    },
    Row {
        case: "a hierarchical parameter tagged after the fact",
        kind: Kind::Parameter,
        name: "/Dev/DBServer/MySQL/db-string13",
        seeded: &[],
        written: &[("Project", "phoenix"), ("Environment", "prod")],
        settled: &[("Environment", "prod"), ("Project", "phoenix")],
    },
    Row {
        case: "a write that overwrites a seeded key and empties another",
        kind: Kind::Parameter,
        name: "native-overwritten",
        seeded: &[("Project", "phoenix"), ("Owner", "platform")],
        written: &[("Project", "atlas"), ("Owner", "")],
        settled: &[("Owner", ""), ("Project", "atlas")],
    },
    Row {
        case: "a parameter nothing ever tagged",
        kind: Kind::Parameter,
        name: "native-untagged",
        seeded: &[],
        written: &[],
        settled: &[],
    },
    Row {
        case: "a document tagged by its own create",
        kind: Kind::Document,
        name: "Native-Document-One",
        seeded: &[("Project", "phoenix")],
        written: &[],
        settled: &[("Project", "phoenix")],
    },
    Row {
        case: "its sibling, carrying the same key under another value, added to later",
        kind: Kind::Document,
        name: "Native-Document-Two",
        seeded: &[("Project", "atlas")],
        written: &[("Owner", "platform")],
        settled: &[("Owner", "platform"), ("Project", "atlas")],
    },
    Row {
        case: "a patch baseline tagged by its own create",
        kind: Kind::PatchBaseline,
        name: "native-baseline",
        seeded: &[("Project", "phoenix")],
        written: &[],
        settled: &[("Project", "phoenix")],
    },
];

/// The tag set the parameter of [`OTHER_REGION`] carries, keyed the same way as the estate
/// of [`REGION`] so that a Region leak would show up as a selection.
const OTHER_REGION_TAGS: &[(&str, &str)] = &[("Project", "phoenix")];
const OTHER_REGION_PARAMETER: &str = "native-other-region";

/// One remove over the settled estate: the row to untag, the keys the remove names, and the
/// tag set that resource is left with — empty where it holds no tag afterwards and is
/// therefore in no answer at all.
struct Removal {
    case: &'static str,
    name: &'static str,
    keys: &'static [&'static str],
    left: &'static [(&'static str, &'static str)],
}

/// The removes, applied in order: the document the issue removes a tag from (whose sibling
/// carries the same key and must keep its entry), the parameter the issue cites, one key of
/// a resource carrying two, every key of a resource carrying two, and a key the resource
/// does not carry.
const REMOVALS: &[Removal] = &[
    Removal {
        case: "the document the issue removes a tag from, leaving its sibling",
        name: "Native-Document-One",
        keys: &["Project"],
        left: &[],
    },
    Removal {
        case: "the parameter the issue cites",
        name: "native-create-one",
        keys: &["Project"],
        left: &[],
    },
    Removal {
        case: "one key of a resource carrying two",
        name: "/Dev/DBServer/MySQL/db-string13",
        keys: &["Project"],
        left: &[("Environment", "prod")],
    },
    Removal {
        case: "every key of a resource carrying two",
        name: "native-overwritten",
        keys: &["Owner", "Project"],
        left: &[],
    },
    Removal {
        case: "a key the resource does not carry leaves its tags as they were",
        name: "Native-Document-Two",
        keys: &["no-such-key"],
        left: &[("Owner", "platform"), ("Project", "atlas")],
    },
];

/// Create one row's resource in `region`, carrying the `Tags` its create declares, and
/// answer the tag-plane `ResourceId` it is addressed by together with the ARN the index has
/// to key it under.
async fn create(router: &MultiServiceRouter, region: &str, row: &Row) -> (String, String) {
    match row.kind {
        Kind::Parameter => {
            let mut request = json!({
                "Name": row.name,
                "Type": "String",
                "Value": "a value",
            });
            if !row.seeded.is_empty() {
                request["Tags"] = tag_list(row.seeded);
            }
            ssm_ok(router, region, "PutParameter", request).await;
            // A parameter's ARN carries its whole hierarchy after the `parameter/` prefix,
            // with the leading separator of the name dropped.
            let path = row.name.trim_start_matches('/');
            (
                row.name.to_string(),
                format!("arn:aws:ssm:{region}:{ACCOUNT_ID}:parameter/{path}"),
            )
        }
        Kind::Document => {
            let mut request = json!({ "Name": row.name, "Content": DOCUMENT_CONTENT });
            if !row.seeded.is_empty() {
                request["Tags"] = tag_list(row.seeded);
            }
            ssm_ok(router, region, "CreateDocument", request).await;
            (
                row.name.to_string(),
                format!("arn:aws:ssm:{region}:{ACCOUNT_ID}:document/{}", row.name),
            )
        }
        Kind::PatchBaseline => {
            let mut request = json!({
                "Name": row.name,
                "OperatingSystem": "WINDOWS",
                // A baseline has to approve something, so the create names one patch.
                "ApprovedPatches": ["KB2124261"],
            });
            if !row.seeded.is_empty() {
                request["Tags"] = tag_list(row.seeded);
            }
            let created = ssm_ok(router, region, "CreatePatchBaseline", request).await;
            let baseline_id = created["BaselineId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: CreatePatchBaseline reports a BaselineId, got {created}",
                        row.case
                    )
                })
                .to_string();
            let arn = format!("arn:aws:ssm:{region}:{ACCOUNT_ID}:patchbaseline/{baseline_id}");
            (baseline_id, arn)
        }
    }
}

/// The estate once every write has landed: each row's tag-plane id and ARN by name, and the
/// `(ARN -> tags)` map the index has to report for the Region, which is every row that ends
/// up carrying at least one tag.
struct Estate {
    ids: BTreeMap<&'static str, String>,
    arns: BTreeMap<&'static str, String>,
    tagged: BTreeMap<String, BTreeMap<String, String>>,
}

/// Create the estate of [`REGION`] and the one parameter of [`OTHER_REGION`], apply each
/// row's `AddTagsToResource`, and wait until the index reports exactly the tag sets the
/// writes left in each Region.
async fn settled_estate(router: &MultiServiceRouter) -> (Estate, String) {
    let mut ids = BTreeMap::new();
    let mut arns = BTreeMap::new();
    let mut tagged = BTreeMap::new();
    for row in ROWS {
        let (id, arn) = create(router, REGION, row).await;
        if !row.written.is_empty() {
            ssm_ok(
                router,
                REGION,
                "AddTagsToResource",
                json!({
                    "ResourceType": row.kind.tagging_type(),
                    "ResourceId": &id,
                    "Tags": tag_list(row.written),
                }),
            )
            .await;
        }
        if !row.settled.is_empty() {
            tagged.insert(arn.clone(), tag_map(row.settled));
        }
        ids.insert(row.name, id);
        arns.insert(row.name, arn);
    }

    let elsewhere = Row {
        case: "the parameter of the other Region",
        kind: Kind::Parameter,
        name: OTHER_REGION_PARAMETER,
        seeded: OTHER_REGION_TAGS,
        written: &[],
        settled: OTHER_REGION_TAGS,
    };
    let (_, elsewhere_arn) = create(router, OTHER_REGION, &elsewhere).await;

    let want = tagged.clone();
    poll_until(
        "the whole estate",
        "the index entries of the resources this Region holds",
        || async {
            let body = tagging_ok(router, REGION, "GetResources", json!({})).await;
            let reported = page_estate(&body);
            if reported == want {
                Ok(())
            } else {
                Err(format!("{reported:?}, expected {want:?}"))
            }
        },
    )
    .await;

    let want_elsewhere: BTreeMap<String, BTreeMap<String, String>> =
        [(elsewhere_arn.clone(), tag_map(OTHER_REGION_TAGS))]
            .into_iter()
            .collect();
    poll_until(
        "the parameter of the other Region",
        "the index entry of that Region's own endpoint",
        || async {
            let body = tagging_ok(router, OTHER_REGION, "GetResources", json!({})).await;
            let reported = page_estate(&body);
            if reported == want_elsewhere {
                Ok(())
            } else {
                Err(format!("{reported:?}, expected {want_elsewhere:?}"))
            }
        },
    )
    .await;

    (Estate { ids, arns, tagged }, elsewhere_arn)
}

/// Every filter form, over the settled estate: `GetResources` selects the SSM resources
/// whose OWN tag set — the one Systems Manager holds — satisfies the request, and reports
/// that set as the entry's `Tags`.
#[tokio::test]
async fn get_resources_selects_ssm_resources_by_the_tags_systems_manager_holds() {
    let router = emulator();
    let (estate, elsewhere) = settled_estate(&router).await;
    let arn = |name: &str| estate.arns[name].clone();

    // The tag set each selected entry reports is the resource's own, pair for pair.
    let body = tagging_ok(&router, REGION, "GetResources", json!({})).await;
    let reported = page_estate(&body);
    for row in ROWS {
        let case = row.case;
        let row_arn = arn(row.name);
        let want = tag_map(row.settled);
        match reported.get(&row_arn) {
            Some(tags) => assert_eq!(
                *tags, want,
                "case {case:?}: expected the index entry for {row_arn} to carry exactly \
                 {want:?}, got {tags:?}"
            ),
            None => assert!(
                row.settled.is_empty(),
                "case {case:?}: expected {row_arn} to be reported, the page holds {reported:?}"
            ),
        }
        // The tag set SSM's own `ListTagsForResource` reports for the resource is the same
        // one, because there is ONE tag home — for every type that read resolves an id of.
        // A patch baseline's id is not one it resolves (`state::tagging::resource_tags`
        // holds no arm for it), so its row has no second surface to compare against here.
        if row.kind != Kind::PatchBaseline {
            let echoed = listed_tags(
                &router,
                REGION,
                row.kind.tagging_type(),
                &estate.ids[row.name],
            )
            .await;
            assert_eq!(
                echoed, want,
                "case {case:?}: expected ListTagsForResource to report {want:?} for \
                 {row_arn}, got {echoed:?}"
            );
        }
    }

    let cases: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "the parameter the issue cites: one key and value under the parameter type",
            json!({
                "ResourceTypeFilters": [PARAMETER_TYPE],
                "TagFilters": [{ "Key": "Project", "Values": ["phoenix"] }],
            }),
            vec![arn("native-create-one"), arn("/Dev/DBServer/MySQL/db-string13")],
        ),
        (
            "the documents the issue cites: a keyless filter under the document type",
            json!({
                "ResourceTypeFilters": [DOCUMENT_TYPE],
                "TagFilters": [{ "Key": "Project" }],
            }),
            vec![arn("Native-Document-One"), arn("Native-Document-Two")],
        ),
        (
            "the same key and value with no type narrowing",
            json!({ "TagFilters": [{ "Key": "Project", "Values": ["phoenix"] }] }),
            vec![
                arn("/Dev/DBServer/MySQL/db-string13"),
                arn("Native-Document-One"),
                arn("native-baseline"),
                arn("native-create-one"),
            ],
        ),
        (
            "several values of one filter are OR-ed",
            json!({ "TagFilters": [{ "Key": "Project", "Values": ["atlas", "phoenix"] }] }),
            vec![
                arn("/Dev/DBServer/MySQL/db-string13"),
                arn("Native-Document-One"),
                arn("Native-Document-Two"),
                arn("native-baseline"),
                arn("native-create-one"),
                arn("native-overwritten"),
            ],
        ),
        (
            "two filters are AND-ed",
            json!({
                "TagFilters": [
                    { "Key": "Project", "Values": ["phoenix"] },
                    { "Key": "Environment", "Values": ["prod"] },
                ],
            }),
            vec![arn("/Dev/DBServer/MySQL/db-string13")],
        ),
        (
            "an AND nothing satisfies",
            json!({
                "TagFilters": [
                    { "Key": "Project", "Values": ["atlas"] },
                    { "Key": "Environment", "Values": ["prod"] },
                ],
            }),
            Vec::new(),
        ),
        (
            "an empty tag value is a value a filter selects by",
            json!({ "TagFilters": [{ "Key": "Owner", "Values": [""] }] }),
            vec![arn("native-overwritten")],
        ),
        (
            "the patch baseline under its own qualified type",
            json!({ "ResourceTypeFilters": [PATCH_BASELINE_TYPE] }),
            vec![arn("native-baseline")],
        ),
        (
            "the bare service token selects every tagged resource of the service",
            json!({ "ResourceTypeFilters": [BARE_TYPE] }),
            estate.tagged.keys().cloned().collect(),
        ),
        (
            "a qualified type no tagged resource answers to",
            json!({ "ResourceTypeFilters": ["ssm:maintenancewindow"] }),
            Vec::new(),
        ),
        (
            "a value nothing carries",
            json!({ "TagFilters": [{ "Key": "Project", "Values": ["no-such-project"] }] }),
            Vec::new(),
        ),
        (
            "no filter at all reports every tagged resource of the Region",
            json!({}),
            estate.tagged.keys().cloned().collect(),
        ),
        // "If a resource specified by this parameter doesn't exist, it doesn't generate an
        // error; it simply isn't included in the response" (`ResourceARNList`'s own
        // documentation) and "GetResources does not return untagged resources" (the
        // operation's): naming the untagged parameter is neither an error nor an entry.
        (
            "ResourceARNList naming every resource of the Region, tagged or not",
            json!({
                "ResourceARNList": ROWS
                    .iter()
                    .map(|row| arn(row.name))
                    .collect::<Vec<String>>(),
            }),
            estate.tagged.keys().cloned().collect(),
        ),
        (
            "ResourceARNList naming the parameter of another Region",
            json!({ "ResourceARNList": [elsewhere.clone()] }),
            Vec::new(),
        ),
    ];

    for (case, request, mut want) in cases {
        want.sort();
        let (got, body) = selected(&router, REGION, request.clone()).await;
        assert_eq!(
            got, want,
            "case {case:?}: expected {request} in {REGION} to select {want:?}, got {got:?} \
             from {body}"
        );
    }

    // The other Region's endpoint answers for its own parameter and for no resource of
    // [`REGION`], under a filter that selects four of them there.
    let case = "the Region scope of the answer";
    let request = json!({ "TagFilters": [{ "Key": "Project", "Values": ["phoenix"] }] });
    let (got, body) = selected(&router, OTHER_REGION, request.clone()).await;
    assert_eq!(
        got,
        vec![elsewhere.clone()],
        "case {case:?}: expected {request} in {OTHER_REGION} to select {elsewhere} alone, got \
         {got:?} from {body}"
    );
}

/// `RemoveTagsFromResource` withdraws exactly the resource it names: the key it drops is
/// gone from that resource's entry — and the entry itself once the resource holds no tag at
/// all — while a sibling carrying the same key keeps its own.
#[tokio::test]
async fn removing_a_tag_withdraws_only_that_resource_from_the_index() {
    let router = emulator();
    let (estate, _) = settled_estate(&router).await;

    let mut expected = estate.tagged.clone();
    for removal in REMOVALS {
        let (case, name) = (removal.case, removal.name);
        let row = ROWS
            .iter()
            .find(|row| row.name == name)
            .unwrap_or_else(|| panic!("case {case:?}: {name} is a row of the estate"));
        ssm_ok(
            &router,
            REGION,
            "RemoveTagsFromResource",
            json!({
                "ResourceType": row.kind.tagging_type(),
                "ResourceId": &estate.ids[name],
                "TagKeys": removal.keys,
            }),
        )
        .await;

        let arn = estate.arns[name].clone();
        if removal.left.is_empty() {
            expected.remove(&arn);
        } else {
            expected.insert(arn.clone(), tag_map(removal.left));
        }
        let want = expected.clone();
        poll_until(case, "the index after the remove", || async {
            let body = tagging_ok(&router, REGION, "GetResources", json!({})).await;
            let reported = page_estate(&body);
            if reported == want {
                Ok(())
            } else {
                Err(format!("{reported:?}, expected {want:?}"))
            }
        })
        .await;

        // The remove edited the resource's OWN tag set, which is what SSM's own read
        // reports: one home, two surfaces.
        let echoed = listed_tags(&router, REGION, row.kind.tagging_type(), &estate.ids[name]).await;
        let want_left = tag_map(removal.left);
        assert_eq!(
            echoed, want_left,
            "case {case:?}: expected ListTagsForResource to report {want_left:?} for {arn}, \
             got {echoed:?}"
        );
    }
}

/// The index SUMMARISES the same owner-held tags: `GetTagKeys` reports every distinct key
/// the Region's tagged SSM resources carry and `GetTagValues` every distinct value under one
/// key — each once, however many resources carry it — and neither reports a key or value
/// that only a resource of another Region carries.
#[tokio::test]
async fn get_tag_keys_and_values_summarise_the_tags_systems_manager_holds() {
    let router = emulator();
    let (estate, _) = settled_estate(&router).await;

    // What the estate's own declarations say the summary is, computed from the rows rather
    // than read back out of the answer.
    let mut want_keys: Vec<String> = Vec::new();
    let mut want_values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for tags in estate.tagged.values() {
        for (key, value) in tags {
            if !want_keys.contains(key) {
                want_keys.push(key.clone());
            }
            let values = want_values.entry(key.clone()).or_default();
            if !values.contains(value) {
                values.push(value.clone());
            }
        }
    }
    want_keys.sort();

    let case = "every key the Region's tagged resources carry";
    let body = tagging_ok(&router, REGION, "GetTagKeys", json!({})).await;
    let got = string_list(&body, "TagKeys");
    assert_eq!(
        got, want_keys,
        "case {case:?}: expected GetTagKeys in {REGION} to report {want_keys:?}, got {got:?}"
    );

    for (key, mut want) in want_values {
        want.sort();
        let body = tagging_ok(&router, REGION, "GetTagValues", json!({ "Key": &key })).await;
        let got = string_list(&body, "TagValues");
        assert_eq!(
            got, want,
            "case {key:?}: expected GetTagValues({key:?}) in {REGION} to report {want:?}, got \
             {got:?}"
        );
    }

    // The other Region's endpoint summarises its own parameter's tags only.
    let case = "the Region scope of the summary";
    let body = tagging_ok(&router, OTHER_REGION, "GetTagKeys", json!({})).await;
    let got = string_list(&body, "TagKeys");
    let want: Vec<String> = OTHER_REGION_TAGS
        .iter()
        .map(|(key, _)| (*key).to_string())
        .collect();
    assert_eq!(
        got, want,
        "case {case:?}: expected GetTagKeys in {OTHER_REGION} to report {want:?}, got {got:?}"
    );
}

/// Every string of a list member of an answer, sorted.
fn string_list(body: &Value, member: &str) -> Vec<String> {
    let mut values: Vec<String> = body[member]
        .as_array()
        .unwrap_or_else(|| panic!("an answer with no {member}: {body}"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("a non-string entry of {member}: {body}"))
                .to_string()
        })
        .collect();
    values.sort();
    values
}
