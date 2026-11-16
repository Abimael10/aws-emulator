// SPDX-License-Identifier: CC0-1.0

//! The OpsMetadata objects Application Manager holds, end to end: `CreateOpsMetadata`
//! registers one for an application, `GetOpsMetadata` reports what it holds and
//! `DeleteOpsMetadata` removes it.
//!
//! RULE: an object is reached by the `OpsMetadataArn` the create MINTED — no input member
//! carries one — so what the create persisted is what a later read reports, a second create
//! for the same application is `OpsMetadataAlreadyExistsException` rather than a second
//! object, and an ARN nothing created names no object. The two ways an ARN names no object of
//! this requester's are different refusals, and the order between them is what keeps a caller
//! from learning what another account holds: an ARN naming another ACCOUNT is
//! `OpsMetadataInvalidArgumentException` before the store is looked at, and an ARN of this
//! account that this Region holds no object under is `OpsMetadataNotFoundException`
//! (`smithy_query(query="operation", operation="GetOpsMetadata")` for both classes).
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`call`], [`call_in_region`], [`error_code`], [`error_message`],
//! [`router`]) dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>` through the
//! multi-service router. Nothing reaches into the state struct, so a response that is not
//! computed from persisted state cannot pass: every read follows a write.
//!
//! The rows span what the rule generalises over: the form of the resource id (a bare name, an
//! ARN of another service, a name under a path), how many metadata entries an object carries
//! and at the published bounds, the Region the object is held at, and the account, Region and
//! path an ARN names.

use serde_json::{json, Value};

use crate::parameter_store::{
    call, call_in_region, error_code, error_message, router, ACCOUNT, REGION,
};

/// A second Region, to pin that an object is reached through the one Region its create was
/// signed for. The first is the harness's own [`REGION`].
const OTHER_REGION: &str = "us-east-1";

/// An account other than the one this emulator serves, for the ARNs that name one.
const OTHER_ACCOUNT: &str = "123456789012";

/// The sentence an ARN of this account that names no object is refused with, written out here
/// independently of the service: fixed text for the class, with nothing of the request in it.
const NOT_FOUND: &str =
    "OpsMetadata object not found. Verify the Amazon Resource Name (ARN) and try again.";

/// The sentence an ARN naming another account is refused with, on the same terms.
const NOT_THE_OWNER: &str = "Invalid OpsMetadataArn. Requester is not the owner of the \
                             resource. Verify the Amazon Resource Name (ARN) and try again.";

/// The published ceiling of `MetadataMap` (`smithy_query(query="operation",
/// operation="CreateOpsMetadata")`: `Metadata` has `length 1..5`).
const METADATA_MAX: usize = 5;

/// The ARN an object registered for the application `name` in `region` is reached by, written
/// out here independently of the service: the `opsmetadata` resource of this account, under
/// the path Application Manager mints one in.
fn ops_metadata_arn(region: &str, name: &str) -> String {
    format!("arn:aws:ssm:{region}:{ACCOUNT}:opsmetadata/aws/ssm/{name}/opsmetadata")
}

/// `Metadata` carrying `count` entries, keyed `Key0..`.
fn metadata(count: usize) -> Value {
    let mut map = serde_json::Map::new();
    for index in 0..count {
        map.insert(
            format!("Key{index}"),
            json!({ "Value": format!("value-{index}") }),
        );
    }
    Value::Object(map)
}

/// RULE: a create mints the ARN from the application the resource id names, and everything a
/// read reports is what that create persisted.
#[tokio::test]
async fn a_created_object_is_read_back_under_the_arn_the_create_minted() {
    /// One row: the resource id a create names, the application its ARN carries, and how
    /// many metadata entries it writes.
    struct Row {
        case: &'static str,
        resource_id: String,
        application: &'static str,
        entries: usize,
    }

    let rows = [
        Row {
            case: "a bare application name",
            resource_id: "MyApplication".to_string(),
            application: "MyApplication",
            entries: 1,
        },
        Row {
            case: "an Application Manager application ARN",
            resource_id: format!("arn:aws:resource-groups:{REGION}:{ACCOUNT}:group/Payments"),
            application: "Payments",
            entries: 2,
        },
        Row {
            case: "a CloudFormation stack ARN, whose last segment is the stack id",
            resource_id: format!(
                "arn:aws:cloudformation:{REGION}:{ACCOUNT}:stack/web/ab12cd34"
            ),
            application: "ab12cd34",
            entries: METADATA_MAX,
        },
        Row {
            case: "an application carrying no metadata at all",
            resource_id: "Barebones".to_string(),
            application: "Barebones",
            entries: 0,
        },
    ];

    for row in rows {
        let case = row.case;
        let router = router();
        let mut request = json!({ "ResourceId": row.resource_id });
        if row.entries > 0 {
            request["Metadata"] = metadata(row.entries);
        }

        let (status, created) = call(&router, "CreateOpsMetadata", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the create must be served, got {status} with {created}"
        );
        let arn = ops_metadata_arn(REGION, row.application);
        assert_eq!(
            created["OpsMetadataArn"].as_str(),
            Some(arn.as_str()),
            "case {case:?}: the ARN carries the application the resource id names, got {created}"
        );

        let (status, read) = call(
            &router,
            "GetOpsMetadata",
            &json!({ "OpsMetadataArn": arn }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must be served, got {status} with {read}"
        );
        assert_eq!(
            read["ResourceId"].as_str(),
            Some(row.resource_id.as_str()),
            "case {case:?}: the read reports the resource id the create supplied, got {read}"
        );
        assert_eq!(
            read["Metadata"],
            metadata(row.entries),
            "case {case:?}: the read reports the metadata the create supplied, got {read}"
        );
        assert!(
            read["NextToken"].is_null(),
            "case {case:?}: one page holds every entry, so no token is handed out, got {read}"
        );
    }
}

/// RULE: a second create for the same application is refused — the ARN is derived from the
/// application, so an object already held under it is that application's own — and the
/// refused create changes nothing.
#[tokio::test]
async fn a_second_create_for_one_application_is_refused() {
    let router = router();
    let request = json!({ "ResourceId": "MyApplication", "Metadata": metadata(1) });
    let (status, created) = call(&router, "CreateOpsMetadata", &request).await;
    assert_eq!(
        status, 200,
        "the first create must be served, got {status} with {created}"
    );

    let (status, refused) = call(
        &router,
        "CreateOpsMetadata",
        &json!({ "ResourceId": "MyApplication", "Metadata": metadata(2) }),
    )
    .await;
    assert_eq!(
        status, 400,
        "the second create must be refused, got {status} with {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "OpsMetadataAlreadyExistsException",
        "expected the already-exists refusal, got {refused}"
    );

    // The object is the one the FIRST create wrote.
    let (_, read) = call(
        &router,
        "GetOpsMetadata",
        &json!({ "OpsMetadataArn": ops_metadata_arn(REGION, "MyApplication") }),
    )
    .await;
    assert_eq!(
        read["Metadata"],
        metadata(1),
        "a refused create writes nothing, got {read}"
    );
}

/// RULE: an object is held at the Region whose endpoint created it, so the same application
/// registered through two Regions is two objects, each reached only through its own.
#[tokio::test]
async fn an_object_is_reached_through_the_region_that_created_it() {
    let router = router();
    for (region, entries) in [(REGION, 1), (OTHER_REGION, 3)] {
        let (status, created) = call_in_region(
            &router,
            region,
            "CreateOpsMetadata",
            &json!({ "ResourceId": "MyApplication", "Metadata": metadata(entries) }),
        )
        .await;
        assert_eq!(
            status, 200,
            "the create at {region} must be served, got {status} with {created}"
        );
        assert_eq!(
            created["OpsMetadataArn"].as_str(),
            Some(ops_metadata_arn(region, "MyApplication").as_str()),
            "the ARN carries the Region its create was signed for, got {created}"
        );
    }

    // (case, the Region the read is signed for, the ARN it names, the entries expected)
    let rows: [(&str, &str, String, usize); 2] = [
        (
            "the Region the object was created through",
            REGION,
            ops_metadata_arn(REGION, "MyApplication"),
            1,
        ),
        (
            "the other Region's own object",
            OTHER_REGION,
            ops_metadata_arn(OTHER_REGION, "MyApplication"),
            3,
        ),
    ];
    for (case, region, arn, entries) in rows {
        let (status, read) = call_in_region(
            &router,
            region,
            "GetOpsMetadata",
            &json!({ "OpsMetadataArn": arn }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must be served, got {status} with {read}"
        );
        assert_eq!(
            read["Metadata"],
            metadata(entries),
            "case {case:?}: each Region reports its own object, got {read}"
        );
    }

    // An ARN naming the OTHER Region's object, asked at this Region's endpoint, names
    // nothing here.
    let (status, body) = call(
        &router,
        "GetOpsMetadata",
        &json!({ "OpsMetadataArn": ops_metadata_arn(OTHER_REGION, "MyApplication") }),
    )
    .await;
    assert_eq!(
        status, 400,
        "an ARN of another Region must be refused here, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "OpsMetadataNotFoundException",
        "expected the not-found refusal, got {body}"
    );
}

/// RULE: the two refusal classes, over the ARNs each covers, and asserted the SAME way for
/// the read and the delete — both resolve the same ARN, so both classify it alike.
#[tokio::test]
async fn an_arn_that_names_no_object_of_this_requester_is_classified_by_whose_it_is() {
    /// One row: the ARN a request names, the code it must be refused with and the sentence
    /// that refusal carries.
    struct Row {
        case: &'static str,
        arn: String,
        code: &'static str,
        message: &'static str,
    }

    let rows = [
        Row {
            case: "an ARN of this account and Region that names no object",
            arn: ops_metadata_arn(REGION, "NobodyCreatedThis"),
            code: "OpsMetadataNotFoundException",
            message: NOT_FOUND,
        },
        Row {
            case: "an ARN under a deeper path of this account",
            arn: format!("arn:aws:ssm:{REGION}:{ACCOUNT}:opsmetadata/aws/ssm/a/b/opsmetadata"),
            code: "OpsMetadataNotFoundException",
            message: NOT_FOUND,
        },
        Row {
            case: "an ARN carrying no account at all",
            arn: format!("arn:aws:ssm:{REGION}::opsmetadata/aws/ssm/Nothing/opsmetadata"),
            code: "OpsMetadataNotFoundException",
            message: NOT_FOUND,
        },
        Row {
            case: "an ARN naming another account",
            arn: format!(
                "arn:aws:ssm:{REGION}:{OTHER_ACCOUNT}:opsmetadata/aws/ssm/Theirs/opsmetadata"
            ),
            code: "OpsMetadataInvalidArgumentException",
            message: NOT_THE_OWNER,
        },
        Row {
            case: "an ARN naming another account in another Region",
            arn: format!(
                "arn:aws:ssm:{OTHER_REGION}:{OTHER_ACCOUNT}:opsmetadata/aws/ssm/T/opsmetadata"
            ),
            code: "OpsMetadataInvalidArgumentException",
            message: NOT_THE_OWNER,
        },
    ];

    let router = router();
    // One object the account DOES hold, so a refusal is about the ARN rather than about an
    // empty store.
    let (status, created) = call(
        &router,
        "CreateOpsMetadata",
        &json!({ "ResourceId": "MyApplication", "Metadata": metadata(1) }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the create must be served, got {status} with {created}"
    );

    for row in rows {
        for operation in ["GetOpsMetadata", "DeleteOpsMetadata"] {
            let (status, body) = call(
                &router,
                operation,
                &json!({ "OpsMetadataArn": row.arn }),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {:?} under {operation}: must be refused, got {status} with {body}",
                row.case
            );
            assert_eq!(
                error_code(&body),
                row.code,
                "case {:?} under {operation}: expected {}, got {body}",
                row.case,
                row.code
            );
            assert_eq!(
                error_message(&body),
                row.message,
                "case {:?} under {operation}: expected {:?}, got {:?}",
                row.case,
                row.message,
                error_message(&body)
            );
        }
    }

    // The object the account holds is untouched by every refusal above.
    let (status, read) = call(
        &router,
        "GetOpsMetadata",
        &json!({ "OpsMetadataArn": ops_metadata_arn(REGION, "MyApplication") }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the held object must still be served, got {status} with {read}"
    );
    assert_eq!(
        read["Metadata"],
        metadata(1),
        "a refused request changes nothing, got {read}"
    );
}

/// RULE: a delete removes the object, so the read that served it before answers the
/// not-found refusal afterwards — and a second delete of the same ARN is that refusal too.
#[tokio::test]
async fn a_delete_removes_the_object_the_arn_names() {
    let router = router();
    let (status, created) = call(
        &router,
        "CreateOpsMetadata",
        &json!({ "ResourceId": "MyApplication", "Metadata": metadata(2) }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the create must be served, got {status} with {created}"
    );
    let arn = ops_metadata_arn(REGION, "MyApplication");

    let (status, deleted) = call(
        &router,
        "DeleteOpsMetadata",
        &json!({ "OpsMetadataArn": arn }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the delete must be served, got {status} with {deleted}"
    );

    for operation in ["GetOpsMetadata", "DeleteOpsMetadata"] {
        let (status, body) = call(&router, operation, &json!({ "OpsMetadataArn": arn })).await;
        assert_eq!(
            status, 400,
            "{operation} after the delete must be refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "OpsMetadataNotFoundException",
            "{operation} after the delete expects the not-found refusal, got {body}"
        );
    }

    // The application can be registered again once the object is gone.
    let (status, again) = call(
        &router,
        "CreateOpsMetadata",
        &json!({ "ResourceId": "MyApplication", "Metadata": metadata(1) }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the application can be registered again, got {status} with {again}"
    );
    assert_eq!(
        again["OpsMetadataArn"].as_str(),
        Some(arn.as_str()),
        "the same application mints the same ARN, got {again}"
    );
}

/// RULE: the metadata entries are served one page at a time — `MaxResults` bounds the page
/// and the token a page hands back is the metadata key the next one resumes at, so walking
/// the pages reports every entry exactly once.
#[tokio::test]
async fn the_metadata_entries_are_paged_by_the_key_the_next_page_resumes_at() {
    let router = router();
    let (status, created) = call(
        &router,
        "CreateOpsMetadata",
        &json!({ "ResourceId": "MyApplication", "Metadata": metadata(METADATA_MAX) }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the create must be served, got {status} with {created}"
    );
    let arn = ops_metadata_arn(REGION, "MyApplication");

    // The published range of `MaxResults` is 1..10, so a page of one, a page at the floor's
    // neighbour and a page larger than the object holds all walk the same entries.
    for page_size in [1usize, 2, METADATA_MAX, METADATA_MAX + 1] {
        let mut seen = serde_json::Map::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut request = json!({ "OpsMetadataArn": arn, "MaxResults": page_size });
            if let Some(token) = token.as_deref() {
                request["NextToken"] = json!(token);
            }
            let (status, page) = call(&router, "GetOpsMetadata", &request).await;
            assert_eq!(
                status, 200,
                "case {page_size}: the page must be served, got {status} with {page}"
            );
            let entries = page["Metadata"]
                .as_object()
                .unwrap_or_else(|| panic!("case {page_size}: a page carries Metadata, got {page}"));
            assert!(
                entries.len() <= page_size,
                "case {page_size}: a page holds at most what was asked for, got {page}"
            );
            for (key, value) in entries {
                assert!(
                    seen.insert(key.clone(), value.clone()).is_none(),
                    "case {page_size}: {key} is reported twice, got {page}"
                );
            }
            pages += 1;
            assert!(
                pages <= METADATA_MAX + 1,
                "case {page_size}: the walk must terminate, got {page}"
            );
            match page["NextToken"].as_str() {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            Value::Object(seen),
            metadata(METADATA_MAX),
            "case {page_size}: the walk reports every entry exactly once"
        );
    }

    // A token that is no key of the object names no page of it to resume.
    let (status, body) = call(
        &router,
        "GetOpsMetadata",
        &json!({ "OpsMetadataArn": arn, "NextToken": "NobodyWroteThisKey" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a token naming no entry must be refused, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "OpsMetadataInvalidArgumentException",
        "expected the invalid-argument refusal, got {body}"
    );
}

/// RULE: the ARN carries the application's name, and the modelled `OpsMetadataArn` admits a
/// narrower character set than a `ResourceId` does — so a resource id whose own last segment
/// could not be spelled inside that ARN names no application an object can be registered
/// for.
#[tokio::test]
async fn a_resource_id_that_no_arn_could_carry_is_refused() {
    // (case, the resource id a create names)
    let rows: [(&str, &str); 4] = [
        ("a name carrying a space", "My Application"),
        ("a name carrying a comma", "payments,web"),
        ("an ARN whose last segment is empty", "arn:aws:ssm:::group/"),
        ("a name that is only a separator", "/"),
    ];

    let router = router();
    for (case, resource_id) in rows {
        let (status, body) = call(
            &router,
            "CreateOpsMetadata",
            &json!({ "ResourceId": resource_id, "Metadata": metadata(1) }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the create must be refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "OpsMetadataInvalidArgumentException",
            "case {case:?}: expected the invalid-argument refusal, got {body}"
        );
    }
}
