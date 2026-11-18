// SPDX-License-Identifier: CC0-1.0

//! `ListTagsForResource` and `TagResource`, end to end over the wire.
//!
//! The rule under test: the tags a Redshift Serverless resource carries are STATE, so
//! `ListTagsForResource` reports exactly the tag set stored for the resource its
//! `resourceArn` resolves to — whichever taggable resource kind that ARN names — and an
//! ARN that resolves to no resource of this account in this Region is refused with the
//! modelled `ResourceNotFoundException`.
//!
//! `TagResource` is the write of that same state: it MERGES the submitted set into the
//! stored one — a key the resource already carries takes the submitted value, a key it
//! does not carry is added, every other key is left alone — so the set a later
//! `ListTagsForResource` reports is the merge and not the request. It is refused without
//! storing anything when the ARN resolves to nothing and when the merge would leave the
//! resource holding more tags than one may hold.
//!
//! Every rule is asserted over a TABLE rather than one request: the rows differ along the
//! resource kind (namespace, workgroup, snapshot), the number and shape of the tags
//! (absent, the empty list, one, several, an empty value, the published length bounds of
//! `Tag$key` / `Tag$value`) and the Region the ARNs are minted in. The unresolvable-ARN
//! table differs along what makes each ARN unresolvable: a fresh id, another resource
//! kind, another account, another Region, another partition, a value that is no ARN at
//! all, and a resource that existed and was deleted.
//!
//! The harness (the emulator, the signed call and the error-code reader) is shared with
//! `namespace_workgroup_lifecycle`, which owns it.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_redshiftserverless_redshiftserverless::state::MAX_TAGS_PER_RESOURCE;

use crate::namespace_workgroup_lifecycle::{call, call_in, emulator, error_code, ACCOUNT};

/// One row of the round-trip table: a tag set a create carries, and the Region it is
/// created in.
#[derive(Debug, Clone)]
struct TagCase {
    /// What distinguishes this row.
    label: &'static str,
    /// The Region the row's resources are created in, so its ARNs differ from every
    /// other row's as well as its tags do.
    region: &'static str,
    /// The `tags` member the create carries: `None` leaves the member out entirely.
    tags: Option<Vec<(String, String)>>,
}

impl TagCase {
    /// The tags a reader must report back for a resource this row created: the set the
    /// create carried, and the EMPTY set for a create that carried none.
    fn expected(&self) -> Vec<(String, String)> {
        let mut expected = self.tags.clone().unwrap_or_default();
        expected.sort();
        expected
    }

    /// The `tags` member as a request carries it.
    fn request_tags(&self) -> Option<Value> {
        self.tags.as_ref().map(|tags| {
            Value::Array(
                tags.iter()
                    .map(|(key, value)| json!({ "key": key, "value": value }))
                    .collect(),
            )
        })
    }
}

/// The rows of the round-trip table.
///
/// The tag shapes come from the model's own bounds —
/// `smithy_query(query="shape", shape="Tag")`: `key` is 1..128 characters and `value` is
/// 0..256, so the empty value and the 128/256-character pair below are the published
/// bounds rather than invented sizes.
fn tag_cases() -> Vec<TagCase> {
    vec![
        TagCase {
            label: "no tags member at all",
            region: "us-east-1",
            tags: None,
        },
        TagCase {
            label: "the empty tag list",
            region: "us-east-1",
            tags: Some(Vec::new()),
        },
        TagCase {
            label: "one tag",
            region: "eu-west-2",
            tags: Some(vec![("env".to_string(), "prod".to_string())]),
        },
        TagCase {
            label: "three tags",
            region: "ap-southeast-2",
            tags: Some(vec![
                ("env".to_string(), "staging".to_string()),
                ("team".to_string(), "data-platform".to_string()),
                ("cost-center".to_string(), "1234".to_string()),
            ]),
        },
        TagCase {
            label: "a tag whose value is the empty string",
            region: "us-west-2",
            tags: Some(vec![
                ("retired".to_string(), String::new()),
                ("owner".to_string(), "analytics".to_string()),
            ]),
        },
        TagCase {
            label: "the published key and value length bounds",
            region: "eu-central-1",
            tags: Some(vec![
                ("k".repeat(128), "v".repeat(256)),
                ("a".to_string(), String::new()),
            ]),
        },
    ]
}

/// The tags `ListTagsForResource` reports for `arn`, as a sorted key/value table,
/// asserting the read itself answered 200.
async fn list_tags(
    router: &MultiServiceRouter,
    region: &str,
    arn: &str,
    case: &str,
) -> Vec<(String, String)> {
    let (status, body) = call_in(
        router,
        region,
        "ListTagsForResource",
        json!({ "resourceArn": arn }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: ListTagsForResource on {arn} expected 200, got {status} with {body}"
    );
    tag_table(&body["tags"], case)
}

/// A `tags` member as a sorted key/value table, asserting it is the modelled list of
/// `Tag` structures.
fn tag_table(tags: &Value, case: &str) -> Vec<(String, String)> {
    let entries = tags
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: tags must be a list, got {tags}"));
    let mut table: Vec<(String, String)> = entries
        .iter()
        .map(|tag| {
            let key = tag["key"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: a tag must carry a key, got {tag}"));
            let value = tag["value"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: a tag must carry a value, got {tag}"));
            (key.to_string(), value.to_string())
        })
        .collect();
    table.sort();
    table
}

/// Create `namespace` in `region` with `tags`, answering the created namespace.
async fn create_namespace_with(
    router: &MultiServiceRouter,
    region: &str,
    namespace: &str,
    tags: Option<Value>,
    case: &str,
) -> Value {
    let mut request = json!({ "namespaceName": namespace });
    if let Some(tags) = tags {
        request["tags"] = tags;
    }
    let (status, body) = call_in(router, region, "CreateNamespace", request).await;
    assert_eq!(
        status, 200,
        "case {case}: CreateNamespace {namespace} expected 200, got {status} with {body}"
    );
    body["namespace"].clone()
}

/// Create `workgroup` on `namespace` in `region` with `tags`, answering the created
/// workgroup.
async fn create_workgroup_with(
    router: &MultiServiceRouter,
    region: &str,
    workgroup: &str,
    namespace: &str,
    tags: Option<Value>,
    case: &str,
) -> Value {
    let mut request = json!({ "workgroupName": workgroup, "namespaceName": namespace });
    if let Some(tags) = tags {
        request["tags"] = tags;
    }
    let (status, body) = call_in(router, region, "CreateWorkgroup", request).await;
    assert_eq!(
        status, 200,
        "case {case}: CreateWorkgroup {workgroup} expected 200, got {status} with {body}"
    );
    body["workgroup"].clone()
}

/// Create `snapshot` of `namespace` in `region` with `tags`, answering the created
/// snapshot.
async fn create_snapshot_with(
    router: &MultiServiceRouter,
    region: &str,
    snapshot: &str,
    namespace: &str,
    tags: Option<Value>,
    case: &str,
) -> Value {
    let mut request = json!({ "snapshotName": snapshot, "namespaceName": namespace });
    if let Some(tags) = tags {
        request["tags"] = tags;
    }
    let (status, body) = call_in(router, region, "CreateSnapshot", request).await;
    assert_eq!(
        status, 200,
        "case {case}: CreateSnapshot {snapshot} expected 200, got {status} with {body}"
    );
    body["snapshot"].clone()
}

/// The string member `path` of `value`, or the empty string.
fn text(value: &Value, path: &str) -> String {
    value[path].as_str().unwrap_or_default().to_string()
}

// ---------------------------------------------------------------------------
// Rule 1 — the tags a create carries are what every later read reports, for
// every taggable resource kind.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_tags_a_create_carries_round_trip_through_list_tags_for_resource() {
    let router = emulator();
    for (index, case) in tag_cases().into_iter().enumerate() {
        let expected = case.expected();
        let namespace = format!("tag-ns-{index}");
        let workgroup = format!("tag-wg-{index}");
        let snapshot = format!("tag-snap-{index}");

        let created_namespace = create_namespace_with(
            &router,
            case.region,
            &namespace,
            case.request_tags(),
            case.label,
        )
        .await;
        let created_workgroup = create_workgroup_with(
            &router,
            case.region,
            &workgroup,
            &namespace,
            case.request_tags(),
            case.label,
        )
        .await;
        let created_snapshot = create_snapshot_with(
            &router,
            case.region,
            &snapshot,
            &namespace,
            case.request_tags(),
            case.label,
        )
        .await;

        // The reads go out signed for the Region the row created its resources in, which
        // is the Region its ARNs name.
        let region = case.region;
        // One ARN per taggable resource kind, each the one its own create returned.
        let arns = [
            ("namespace", text(&created_namespace, "namespaceArn")),
            ("workgroup", text(&created_workgroup, "workgroupArn")),
            ("snapshot", text(&created_snapshot, "snapshotArn")),
        ];
        for (kind, arn) in &arns {
            let case = format!("{} / {kind}", case.label);
            assert!(
                !arn.is_empty(),
                "case {case}: the create must return an ARN to read the tags back through"
            );
            let listed = list_tags(&router, region, arn, &case).await;
            assert_eq!(
                listed, expected,
                "case {case}: expected ListTagsForResource to report {expected:?}, got {listed:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 1, continued — one resource's tags are its own, and reading them
// changes nothing a later caller sees.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn each_resource_reports_its_own_tags_and_a_read_changes_nothing() {
    let router = emulator();
    // Three resources in ONE store, each created with a DIFFERENT tag set, so an answer
    // taken from the wrong record cannot pass.
    let namespace_tags = json!([{ "key": "kind", "value": "namespace" }]);
    let workgroup_tags = json!([
        { "key": "kind", "value": "workgroup" },
        { "key": "tier", "value": "compute" },
    ]);
    let snapshot_tags = json!([{ "key": "kind", "value": "snapshot" }, { "key": "keep", "value": "" }]);

    let namespace = create_namespace_with(
        &router,
        "us-east-1",
        "own-tags-ns",
        Some(namespace_tags.clone()),
        "isolation",
    )
    .await;
    let workgroup = create_workgroup_with(
        &router,
        "us-east-1",
        "own-tags-wg",
        "own-tags-ns",
        Some(workgroup_tags.clone()),
        "isolation",
    )
    .await;
    let snapshot = create_snapshot_with(
        &router,
        "us-east-1",
        "own-tags-snap",
        "own-tags-ns",
        Some(snapshot_tags.clone()),
        "isolation",
    )
    .await;

    let rows = [
        (
            "namespace",
            text(&namespace, "namespaceArn"),
            namespace_tags,
        ),
        (
            "workgroup",
            text(&workgroup, "workgroupArn"),
            workgroup_tags,
        ),
        ("snapshot", text(&snapshot, "snapshotArn"), snapshot_tags),
    ];
    for (kind, arn, tags) in &rows {
        let case = format!("isolation / {kind}");
        let expected = tag_table(tags, &case);
        let listed = list_tags(&router, "us-east-1", arn, &case).await;
        assert_eq!(
            listed, expected,
            "case {case}: expected ListTagsForResource to report {expected:?}, got {listed:?}"
        );
        // A read is a read: the second call answers the first one's answer.
        let again = list_tags(&router, "us-east-1", arn, &case).await;
        assert_eq!(
            again, listed,
            "case {case}: a second ListTagsForResource must report {listed:?}, got {again:?}"
        );
    }

    // And every other resource still reports its own set after all those reads.
    for (kind, arn, tags) in &rows {
        let case = format!("isolation after the reads / {kind}");
        let expected = tag_table(tags, &case);
        let listed = list_tags(&router, "us-east-1", arn, &case).await;
        assert_eq!(
            listed, expected,
            "case {case}: expected ListTagsForResource to report {expected:?}, got {listed:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 1, continued — a tag outside its published bounds never becomes state,
// so no read can report it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_tag_outside_its_published_length_bounds_never_becomes_state() {
    // `Tag$key` is 1..128 characters and `Tag$value` 0..256
    // (`smithy_query(query="shape", shape="Tag")`). Each row is one character PAST one of
    // those bounds; the bounds themselves are a row of the round-trip table above.
    let cases: [(&str, Value); 3] = [
        (
            "a key one character past 128",
            json!([{ "key": "k".repeat(129), "value": "v" }]),
        ),
        (
            "a key one character short of 1",
            json!([{ "key": "", "value": "v" }]),
        ),
        (
            "a value one character past 256",
            json!([{ "key": "k", "value": "v".repeat(257) }]),
        ),
    ];

    let router = emulator();
    for (index, (label, tags)) in cases.iter().enumerate() {
        let namespace = format!("bound-ns-{index}");
        let (status, body) = call(
            &router,
            "CreateNamespace",
            json!({ "namespaceName": namespace, "tags": tags }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {label}: CreateNamespace must refuse the tag, got {status} with {body}"
        );

        // Nothing was stored, so nothing resolves the ARN such a create would have
        // returned, and no later read can report that tag.
        let (status, body) = call(
            &router,
            "GetNamespace",
            json!({ "namespaceName": namespace }),
        )
        .await;
        assert_eq!(
            error_code(&body).as_str(),
            "ResourceNotFoundException",
            "case {label}: the refused create stored nothing, got {status} with {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2 — an ARN that resolves to no resource of this account in this Region
// is the modelled ResourceNotFoundException.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_arn_that_resolves_to_no_resource_is_refused_as_not_found() {
    let router = emulator();
    let namespace = create_namespace_with(&router, "us-east-1", "absent-ns", None, "setup").await;
    let workgroup = create_workgroup_with(
        &router,
        "us-east-1",
        "absent-wg",
        "absent-ns",
        None,
        "setup",
    )
    .await;
    let real_namespace_id = text(&namespace, "namespaceId");
    let real_workgroup_id = text(&workgroup, "workgroupId");

    // A namespace that existed and was deleted: its ARN resolved a moment ago and must
    // not resolve now, which is what makes the refusal a fact about STATE.
    let doomed = create_namespace_with(&router, "us-east-1", "doomed-ns", None, "setup").await;
    let doomed_arn = text(&doomed, "namespaceArn");
    let (status, body) = call(
        &router,
        "DeleteNamespace",
        json!({ "namespaceName": "doomed-ns" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "DeleteNamespace doomed-ns expected 200, got {status} with {body}"
    );

    // A UUID the emulator minted for nothing.
    let absent_id = "3f8c1a7e-5b2d-4c6e-9a10-7d4e2f6b8c90";
    let cases: Vec<(&str, String)> = vec![
        (
            "a namespace id that names nothing",
            format!("arn:aws:redshift-serverless:us-east-1:{ACCOUNT}:namespace/{absent_id}"),
        ),
        (
            "a workgroup id that names nothing",
            format!("arn:aws:redshift-serverless:us-east-1:{ACCOUNT}:workgroup/{absent_id}"),
        ),
        (
            "a snapshot id that names nothing",
            format!("arn:aws:redshift-serverless:us-east-1:{ACCOUNT}:snapshot/{absent_id}"),
        ),
        (
            "a real namespace id under the wrong resource kind",
            format!("arn:aws:redshift-serverless:us-east-1:{ACCOUNT}:workgroup/{real_namespace_id}"),
        ),
        (
            "a real workgroup id in another account",
            format!("arn:aws:redshift-serverless:us-east-1:999999999999:workgroup/{real_workgroup_id}"),
        ),
        (
            "a real workgroup id in another Region",
            format!("arn:aws:redshift-serverless:eu-central-1:{ACCOUNT}:workgroup/{real_workgroup_id}"),
        ),
        (
            "a real workgroup id in another partition",
            format!(
                "arn:aws-us-gov:redshift-serverless:us-gov-west-1:{ACCOUNT}:workgroup/\
                 {real_workgroup_id}"
            ),
        ),
        ("a value that is no ARN at all", "not-an-arn".to_string()),
        ("the ARN of a deleted namespace", doomed_arn),
    ];

    for (label, resource_arn) in &cases {
        let case = format!("{label} ({resource_arn})");
        let (status, body) = call(
            &router,
            "ListTagsForResource",
            json!({ "resourceArn": resource_arn }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case}: got {status} with {body}"
        );
        assert_eq!(
            body["message"],
            json!(format!("Resource {resource_arn} not found.")),
            "case {case}: got {:?}",
            body["message"]
        );
        assert_eq!(
            body["resourceName"],
            json!(resource_arn),
            "case {case}: resourceName names the resource that was not found, got {:?}",
            body["resourceName"]
        );
    }

    // The ARNs that DO resolve are served, so the refusal above is about the ARN
    // resolving to nothing and not about this request's shape.
    for (kind, arn) in [
        ("namespace", text(&namespace, "namespaceArn")),
        ("workgroup", text(&workgroup, "workgroupArn")),
    ] {
        let listed = list_tags(&router, "us-east-1", &arn, kind).await;
        assert!(
            listed.is_empty(),
            "case {kind}: a create that carried no tags reports none, got {listed:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 3 — TagResource MERGES the submitted set into the stored one: a key the
// resource already carries takes the submitted value, a key it does not carry
// is added, and every other key is left alone.
// ---------------------------------------------------------------------------

/// One row of the merge table: the tags a create carries, the tags one `TagResource` call
/// submits, and the set every later read must report.
#[derive(Debug, Clone)]
struct MergeCase {
    /// What distinguishes this row.
    label: &'static str,
    /// The Region the row's resources are created in, so its ARNs differ from every other
    /// row's.
    region: &'static str,
    /// The `tags` the create carries.
    created: Vec<(String, String)>,
    /// The `tags` the `TagResource` call submits.
    submitted: Vec<(String, String)>,
    /// The merged set, in any order.
    expected: Vec<(String, String)>,
}

/// A key/value table as a `tags` member.
fn tag_list(tags: &[(String, String)]) -> Value {
    Value::Array(
        tags.iter()
            .map(|(key, value)| json!({ "key": key, "value": value }))
            .collect(),
    )
}

/// `tags` as a sorted table, the form every assertion here compares.
fn sorted(tags: &[(String, String)]) -> Vec<(String, String)> {
    let mut sorted = tags.to_vec();
    sorted.sort();
    sorted
}

/// `count` distinct tags whose keys all start with `prefix`. Keys stay inside `Tag$key`'s
/// 1..128 bound and values inside `Tag$value`'s 0..256.
fn numbered_tags(prefix: &str, count: usize) -> Vec<(String, String)> {
    (0..count)
        .map(|index| (format!("{prefix}-{index:03}"), format!("value-{index}")))
        .collect()
}

/// `TagResource` for `arn` with `tags`, answering its HTTP status and parsed body.
async fn tag_resource_call(
    router: &MultiServiceRouter,
    region: &str,
    arn: &str,
    tags: &[(String, String)],
) -> (u16, Value) {
    call_in(
        router,
        region,
        "TagResource",
        json!({ "resourceArn": arn, "tags": tag_list(tags) }),
    )
    .await
}

/// [`tag_resource_call`], asserting it was served.
async fn tag_resource(
    router: &MultiServiceRouter,
    region: &str,
    arn: &str,
    tags: &[(String, String)],
    case: &str,
) {
    let (status, body) = tag_resource_call(router, region, arn, tags).await;
    assert_eq!(
        status, 200,
        "case {case}: TagResource on {arn} expected 200, got {status} with {body}"
    );
}

/// The rows of the merge table.
///
/// They differ along what the rule generalises over: whether the resource already carries
/// tags, whether a submitted key is new or already held, how many keys one call carries,
/// whether a call repeats a key, and the shape of the values — including the published
/// bounds of `Tag$key` (1..128) and `Tag$value` (0..256), from
/// `smithy_query(query="shape", shape="Tag")`.
fn merge_cases() -> Vec<MergeCase> {
    let pair = |key: &str, value: &str| (key.to_string(), value.to_string());
    vec![
        MergeCase {
            label: "a key added to a resource created with no tags",
            region: "us-east-1",
            created: Vec::new(),
            submitted: vec![pair("env", "prod")],
            expected: vec![pair("env", "prod")],
        },
        MergeCase {
            label: "keys added beside the ones the create carried",
            region: "eu-west-2",
            created: vec![pair("env", "staging"), pair("team", "data")],
            submitted: vec![pair("owner", "analytics"), pair("cost-center", "1234")],
            expected: vec![
                pair("env", "staging"),
                pair("team", "data"),
                pair("owner", "analytics"),
                pair("cost-center", "1234"),
            ],
        },
        MergeCase {
            label: "a held key takes the submitted value and its siblings are untouched",
            region: "ap-southeast-2",
            created: vec![pair("env", "staging"), pair("team", "data")],
            submitted: vec![pair("env", "prod")],
            expected: vec![pair("env", "prod"), pair("team", "data")],
        },
        MergeCase {
            label: "a held key takes the empty value",
            region: "us-west-2",
            created: vec![pair("retired", "no"), pair("owner", "analytics")],
            submitted: vec![pair("retired", "")],
            expected: vec![pair("retired", ""), pair("owner", "analytics")],
        },
        MergeCase {
            label: "a key one call repeats ends at its last value",
            region: "eu-central-1",
            created: vec![pair("env", "staging")],
            submitted: vec![pair("env", "first"), pair("env", "last")],
            expected: vec![pair("env", "last")],
        },
        MergeCase {
            label: "the empty submitted list leaves the stored set alone",
            region: "us-east-2",
            created: vec![pair("env", "staging"), pair("team", "data")],
            submitted: Vec::new(),
            expected: vec![pair("env", "staging"), pair("team", "data")],
        },
        MergeCase {
            label: "the published key and value length bounds",
            region: "ca-central-1",
            created: vec![pair("env", "staging")],
            submitted: vec![
                ("k".repeat(128), "v".repeat(256)),
                ("a".to_string(), String::new()),
            ],
            expected: vec![
                pair("env", "staging"),
                ("k".repeat(128), "v".repeat(256)),
                ("a".to_string(), String::new()),
            ],
        },
        MergeCase {
            label: "many keys in one call",
            region: "ap-northeast-1",
            created: numbered_tags("created", 3),
            submitted: numbered_tags("added", 12),
            expected: numbered_tags("created", 3)
                .into_iter()
                .chain(numbered_tags("added", 12))
                .collect(),
        },
    ]
}

#[tokio::test]
async fn tag_resource_merges_the_submitted_tags_into_the_stored_set() {
    let router = emulator();
    for (index, case) in merge_cases().into_iter().enumerate() {
        let expected = sorted(&case.expected);
        let namespace = format!("merge-ns-{index}");
        let workgroup = format!("merge-wg-{index}");
        let snapshot = format!("merge-snap-{index}");
        let created = Some(tag_list(&case.created));

        let created_namespace = create_namespace_with(
            &router,
            case.region,
            &namespace,
            created.clone(),
            case.label,
        )
        .await;
        let created_workgroup = create_workgroup_with(
            &router,
            case.region,
            &workgroup,
            &namespace,
            created.clone(),
            case.label,
        )
        .await;
        let created_snapshot = create_snapshot_with(
            &router,
            case.region,
            &snapshot,
            &namespace,
            created.clone(),
            case.label,
        )
        .await;

        // Every taggable resource kind takes the write the same way.
        let arns = [
            ("namespace", text(&created_namespace, "namespaceArn")),
            ("workgroup", text(&created_workgroup, "workgroupArn")),
            ("snapshot", text(&created_snapshot, "snapshotArn")),
        ];
        for (kind, arn) in &arns {
            let case_name = format!("{} / {kind}", case.label);
            tag_resource(&router, case.region, arn, &case.submitted, &case_name).await;
            let listed = list_tags(&router, case.region, arn, &case_name).await;
            assert_eq!(
                listed, expected,
                "case {case_name}: expected the merged set {expected:?}, got {listed:?}"
            );

            // Applying the same set again is the same state: a merge of a set already
            // held changes nothing.
            tag_resource(&router, case.region, arn, &case.submitted, &case_name).await;
            let again = list_tags(&router, case.region, arn, &case_name).await;
            assert_eq!(
                again, expected,
                "case {case_name}: re-applying the same tags must leave {expected:?}, got {again:?}"
            );
        }
    }
}

#[tokio::test]
async fn tagging_one_resource_leaves_every_other_resource_alone() {
    let router = emulator();
    let shared = ("env".to_string(), "staging".to_string());
    let created = Some(tag_list(std::slice::from_ref(&shared)));

    let namespace =
        create_namespace_with(&router, "us-east-1", "only-ns", created.clone(), "isolation").await;
    let workgroup = create_workgroup_with(
        &router,
        "us-east-1",
        "only-wg",
        "only-ns",
        created.clone(),
        "isolation",
    )
    .await;
    let snapshot = create_snapshot_with(
        &router,
        "us-east-1",
        "only-snap",
        "only-ns",
        created.clone(),
        "isolation",
    )
    .await;

    let rows = [
        ("namespace", text(&namespace, "namespaceArn")),
        ("workgroup", text(&workgroup, "workgroupArn")),
        ("snapshot", text(&snapshot, "snapshotArn")),
    ];
    // One resource at a time is tagged with a key naming it. After each write, every
    // resource must report its create's tag and the marker of its OWN write only — so a
    // write that reached the wrong record, or reached more than one, cannot pass.
    let mut tagged_so_far: Vec<&str> = Vec::new();
    for (tagged_kind, tagged_arn) in &rows {
        let case = format!("after tagging the {tagged_kind}");
        let marker = ("tagged".to_string(), (*tagged_kind).to_string());
        tag_resource(
            &router,
            "us-east-1",
            tagged_arn,
            std::slice::from_ref(&marker),
            &case,
        )
        .await;
        tagged_so_far.push(tagged_kind);

        for (kind, arn) in &rows {
            let case = format!("{case} / reading the {kind}");
            let mut expected = vec![shared.clone()];
            if tagged_so_far.contains(kind) {
                expected.push(("tagged".to_string(), (*kind).to_string()));
            }
            let expected = sorted(&expected);
            let listed = list_tags(&router, "us-east-1", arn, &case).await;
            assert_eq!(
                listed, expected,
                "case {case}: expected {expected:?}, got {listed:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 3, continued — a TagResource whose ARN resolves to nothing is the
// modelled ResourceNotFoundException, and stores nothing.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tag_resource_refuses_an_arn_that_resolves_to_no_resource() {
    let router = emulator();
    let namespace =
        create_namespace_with(&router, "us-east-1", "write-ns", None, "setup").await;
    let real_namespace_id = text(&namespace, "namespaceId");
    let namespace_arn = text(&namespace, "namespaceArn");

    // A namespace that existed and was deleted: its ARN resolved a moment ago.
    let doomed =
        create_namespace_with(&router, "us-east-1", "doomed-write-ns", None, "setup").await;
    let doomed_arn = text(&doomed, "namespaceArn");
    let (status, body) = call(
        &router,
        "DeleteNamespace",
        json!({ "namespaceName": "doomed-write-ns" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "DeleteNamespace doomed-write-ns expected 200, got {status} with {body}"
    );

    // A UUID the emulator minted for nothing.
    let absent_id = "9c21f0ab-6d4e-4a1b-8f33-05e7b6c2d914";
    let cases: Vec<(&str, String)> = vec![
        (
            "a namespace id that names nothing",
            format!("arn:aws:redshift-serverless:us-east-1:{ACCOUNT}:namespace/{absent_id}"),
        ),
        (
            "a workgroup id that names nothing",
            format!("arn:aws:redshift-serverless:us-east-1:{ACCOUNT}:workgroup/{absent_id}"),
        ),
        (
            "a real namespace id under the wrong resource kind",
            format!("arn:aws:redshift-serverless:us-east-1:{ACCOUNT}:snapshot/{real_namespace_id}"),
        ),
        (
            "a real namespace id in another account",
            format!("arn:aws:redshift-serverless:us-east-1:999999999999:namespace/{real_namespace_id}"),
        ),
        (
            "a real namespace id in another Region",
            format!("arn:aws:redshift-serverless:eu-central-1:{ACCOUNT}:namespace/{real_namespace_id}"),
        ),
        (
            "a real namespace id in another partition",
            format!(
                "arn:aws-us-gov:redshift-serverless:us-gov-west-1:{ACCOUNT}:namespace/\
                 {real_namespace_id}"
            ),
        ),
        ("a value that is no ARN at all", "not-an-arn".to_string()),
        ("the ARN of a deleted namespace", doomed_arn),
    ];

    let submitted = vec![("env".to_string(), "prod".to_string())];
    for (label, resource_arn) in &cases {
        let case = format!("{label} ({resource_arn})");
        let (status, body) = tag_resource_call(&router, "us-east-1", resource_arn, &submitted).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case}: got {status} with {body}"
        );
        assert_eq!(
            body["message"],
            json!(format!("Resource {resource_arn} not found.")),
            "case {case}: got {:?}",
            body["message"]
        );
        assert_eq!(
            body["resourceName"],
            json!(resource_arn),
            "case {case}: resourceName names the resource that was not found, got {:?}",
            body["resourceName"]
        );
    }

    // None of those refusals wrote anywhere: the one resource that does exist still
    // reports the empty set its create carried.
    let listed = list_tags(&router, "us-east-1", &namespace_arn, "after the refusals").await;
    assert!(
        listed.is_empty(),
        "case after the refusals: the namespace must still carry no tags, got {listed:?}"
    );

    // And the ARN that DOES resolve is served, so the refusals are about the ARN
    // resolving to nothing and not about the request's shape.
    tag_resource(
        &router,
        "us-east-1",
        &namespace_arn,
        &submitted,
        "the ARN that resolves",
    )
    .await;
    let listed = list_tags(&router, "us-east-1", &namespace_arn, "the ARN that resolves").await;
    assert_eq!(
        listed,
        sorted(&submitted),
        "case the ARN that resolves: expected {:?}, got {listed:?}",
        sorted(&submitted)
    );
}

// ---------------------------------------------------------------------------
// Rule 3, continued — the number of tags a resource may hold is a bound on
// STATE, so a merge past it is TooManyTagsException and stores nothing.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_merge_past_the_tag_bound_is_refused_and_stores_nothing() {
    let router = emulator();
    // The bound is the modelled one (`TagList`'s `@length(max: 200)`), read from the
    // crate rather than written out here. The create carries part of the set and one
    // `TagResource` call takes the resource to exactly the bound.
    let created = numbered_tags("created", MAX_TAGS_PER_RESOURCE / 2);
    let namespace = create_namespace_with(
        &router,
        "us-east-1",
        "cap-ns",
        Some(tag_list(&created)),
        "the bound",
    )
    .await;
    let arn = text(&namespace, "namespaceArn");

    let topping = numbered_tags("added", MAX_TAGS_PER_RESOURCE - created.len());
    tag_resource(&router, "us-east-1", &arn, &topping, "exactly the bound").await;
    let at_bound = list_tags(&router, "us-east-1", &arn, "exactly the bound").await;
    assert_eq!(
        at_bound.len(),
        MAX_TAGS_PER_RESOURCE,
        "case exactly the bound: expected {MAX_TAGS_PER_RESOURCE} tags, got {}",
        at_bound.len()
    );

    // One key past the bound, and a whole batch past it, are refused the same way.
    let past: [(&str, Vec<(String, String)>); 2] = [
        ("one key past the bound", numbered_tags("past", 1)),
        ("a batch past the bound", numbered_tags("batch", 25)),
    ];
    for (label, submitted) in &past {
        let (status, body) = tag_resource_call(&router, "us-east-1", &arn, submitted).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "TooManyTagsException"),
            "case {label}: got {status} with {body}"
        );
        assert_eq!(
            body["resourceName"],
            json!(arn),
            "case {label}: resourceName names the resource that exceeded the bound, got {:?}",
            body["resourceName"]
        );
        let after = list_tags(&router, "us-east-1", &arn, label).await;
        assert_eq!(
            after, at_bound,
            "case {label}: the refused call must store nothing, got {} tags",
            after.len()
        );
    }

    // A resource at the bound still takes a value for a key it already carries: that
    // merge adds no key, so it is not past anything.
    let held = vec![(created[0].0.clone(), "re-tagged".to_string())];
    tag_resource(&router, "us-east-1", &arn, &held, "a held key at the bound").await;
    let after = list_tags(&router, "us-east-1", &arn, "a held key at the bound").await;
    assert_eq!(
        after.len(),
        MAX_TAGS_PER_RESOURCE,
        "case a held key at the bound: expected {MAX_TAGS_PER_RESOURCE} tags, got {}",
        after.len()
    );
    assert!(
        after.contains(&held[0]),
        "case a held key at the bound: expected {:?} among the tags, got {after:?}",
        held[0]
    );
}
