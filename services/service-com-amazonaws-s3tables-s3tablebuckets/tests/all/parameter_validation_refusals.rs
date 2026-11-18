// SPDX-License-Identifier: CC0-1.0

//! How S3 Tables refuses a parameter whose SHAPE a modelled constraint rules
//! out, on a member the service states no sentence of its own for.
//!
//! The rule under test: such a request never reaches the service — the parameter
//! validator in front of it refuses it with HTTP 400 under its own error code
//! (`ValidationException`, which names no shape of this model, because the
//! refusal is of the request's shape and not of anything the service decided)
//! and one envelope: `1 validation error detected: Value at '<member path>'
//! failed to satisfy constraint: <requirement>`, counted in the plural when the
//! request breaks more than one. The clause states WHERE the value sat and WHAT
//! it had to satisfy, and echoes no value, so the same refusal reads identically
//! whatever was sent. The members the service DOES word itself — a name a create
//! gives a resource, an ARN an operation is addressed by, a table format, a
//! configuration's status, an encryption configuration — are the other layer:
//! they carry this model's
//! `BadRequestException` and a fixed sentence, which `constraint_refusals.rs`,
//! `invalid_input_refusals.rs`, `maintenance_request_refusals.rs` and
//! `encryption_configuration_refusals.rs` pin.
//!
//! Each table spans the class the rule generalises over rather than one recorded
//! request: which CONSTRAINT the value broke (a character set, a length bound at
//! either end, a numeric bound, an enumerated set), how DEEP in the request the
//! member sat (an input member, a member of a nested structure, an element of a
//! list of structures inside a list of structures, and which element), and how
//! many constraints one request breaks at once. The requirements and bounds
//! quoted below are the model's own
//! (`smithy_query(query="shape", shape="TableBucketReplicationConfiguration,
//! ReplicationDestination,TableRecordExpirationSettings,
//! StorageClass,VersionToken,IAMRole")`).
//!
//! One half of the enumerated-set requirement is an ORDER rather than a
//! sentence: the bracketed list states the values the type admits SORTED,
//! whatever order the model declares them in, and the order belongs to the
//! enumerated TYPE rather than to the member, the operation, the depth or the
//! request that carried the value. The rows for it span two of this model's types
//! (each declared in an order that is NOT its sorted one), the three operations
//! that carry one of them in the same nested structure, a query member for the
//! other, a resource the call addresses and one nothing created, and the
//! spellings a refused value arrives in; a test of its own reads the set off the
//! service's own message and holds it to the sorted order of the model's
//! declaration rather than to a literal.
//!
//! The accepted rows keep the refusals honest: the value AT each published bound
//! is served, so what is refused is the values outside the bound and not the
//! whole member — and for the enumerated type, the values it DOES admit are put
//! and read back, while a refused one leaves what a later read reports as it was.

use serde_json::{json, Value};

use crate::harness::{encode, expected_bucket_arn, query_string, Emulator};
use crate::table_record_expiration::{create_table, mark_managed};

/// The region every case works in.
const REGION: &str = "us-east-1";

/// The code the parameter validator answers with.
const PARAMETER_VALIDATION: &str = "ValidationException";

/// `TableBucketARN`'s pattern as the requirement states it: the model writes
/// `^(…)$`, and the anchors state the member's position rather than anything the
/// value must carry.
const BUCKET_ARN_PATTERN: &str =
    "(arn:aws[-a-z0-9]*:[a-z0-9]+:[-a-z0-9]*:[0-9]{12}:bucket/[a-z0-9_-]{3,63})";

/// `IAMRole`'s pattern, the same way.
const IAM_ROLE_PATTERN: &str = "arn:.+:iam::[0-9]{12}:role/.+";

/// An IAM role ARN of the form `IAMRole` accepts, for the rows that refuse
/// something else.
const ROLE: &str = "arn:aws:iam::000000000000:role/s3tables-replication";

/// The bracketed set a clause about a `StorageClass` member lists: the values
/// that type admits, SORTED. The model declares them `STANDARD`,
/// `INTELLIGENT_TIERING` (`smithy_query(query="shape", shape="StorageClass")`),
/// so the order below is the one this service states the set in and not the
/// declaration's.
const STORAGE_CLASS_SET: &str = "[INTELLIGENT_TIERING, STANDARD]";

/// The same for `TableBucketType` — the second type this model declares in an
/// order of its own (`customer`, `aws`, by
/// `smithy_query(query="shape", shape="TableBucketType")`), and the one carried
/// by a QUERY member rather than inside a body.
const TABLE_BUCKET_TYPE_SET: &str = "[aws, customer]";

/// `StorageClass`'s values in the order the MODEL declares them, for the test
/// that holds a stated set to the sorted order of the declaration rather than to
/// a literal.
const STORAGE_CLASS_DECLARED: [&str; 2] = ["STANDARD", "INTELLIGENT_TIERING"];

/// `TableBucketType`'s values, the same way.
const TABLE_BUCKET_TYPE_DECLARED: [&str; 2] = ["customer", "aws"];

/// The requirement an enum constraint states over `value_set`.
fn enum_requirement(value_set: &str) -> String {
    format!("Member must satisfy enum value set: {value_set}")
}

/// The spellings of a value outside `StorageClass` a caller can send: a name no
/// storage class carries, an admitted value in the wrong case, a name of another
/// service's storage tier, and the empty string.
const REFUSED_STORAGE_CLASSES: [&str; 5] = [
    "ULTRA_FAST_TIER",
    "NOT_A_REAL_STORAGE_CLASS",
    "standard",
    "GLACIER",
    "",
];

/// The spellings of a value outside `TableBucketType` a caller can send.
const REFUSED_BUCKET_TYPES: [&str; 3] = ["ULTRA", "AWS", "Customer"];

/// One refused request: how it is addressed, which member path the refusal must
/// name, the requirement it must state, and the values the request carried that
/// must appear nowhere in it.
struct Case {
    label: String,
    method: &'static str,
    path: String,
    body: Option<Value>,
    member_path: &'static str,
    requirement: String,
    sent: Vec<String>,
}

impl Case {
    /// The whole sentence this refusal must carry.
    fn expected_message(&self) -> String {
        format!(
            "1 validation error detected: Value at '{member}' failed to satisfy constraint: \
             {requirement}",
            member = self.member_path,
            requirement = self.requirement
        )
    }
}

/// A `PutTableBucketReplication` request addressed to `bucket`, carrying
/// `configuration`.
fn replication_put(bucket: &str, configuration: Value) -> (String, Option<Value>) {
    (
        format!(
            "/table-bucket-replication{}",
            query_string(&[("tableBucketARN", bucket.to_string())])
        ),
        Some(json!({ "configuration": configuration })),
    )
}

/// A `TableBucketReplicationConfiguration` whose one rule replicates to
/// `destinations`, in that order.
fn configuration(role: &str, destinations: &[&str]) -> Value {
    json!({
        "role": role,
        "rules": [{
            "destinations": destinations
                .iter()
                .map(|arn| json!({ "destinationTableBucketARN": arn }))
                .collect::<Vec<Value>>(),
        }],
    })
}

/// Every refusal the parameter validator makes, across the constraint classes
/// and the depths the rule generalises over.
fn cases(bucket: &str, table_arn: &str) -> Vec<Case> {
    let mut cases = Vec::new();

    // A `@pattern` on a member sat two collections deep: the destination of a
    // rule of a configuration. The spellings that cannot be a table bucket ARN:
    // nothing ARN-shaped at all, an account id one digit short of the modelled
    // twelve, a bucket segment under the modelled three characters, and the ARN
    // of a table rather than a bucket.
    let malformed_arns = [
        "not-an-arn".to_string(),
        format!("arn:aws:s3tables:{REGION}:00000000000:bucket/target"),
        format!("arn:aws:s3tables:{REGION}:000000000000:bucket/ab"),
        format!("{}/table/0123456789abcdef", expected_bucket_arn(REGION, "target")),
    ];
    for malformed in &malformed_arns {
        let (path, body) = replication_put(bucket, configuration(ROLE, &[malformed]));
        cases.push(Case {
            label: format!("the only destination spelled {malformed:?}"),
            method: "PUT",
            path,
            body,
            member_path: "configuration.rules.1.member.destinations.1.member.\
                          destinationTableBucketARN",
            requirement: format!("Member must satisfy regular expression pattern: {BUCKET_ARN_PATTERN}"),
            sent: vec![malformed.clone()],
        });
    }

    // The SECOND destination of the same rule: the clause names the element that
    // failed, so the position travels with it.
    let well_formed = expected_bucket_arn(REGION, "replication-target");
    let (path, body) = replication_put(
        bucket,
        configuration(ROLE, &[&well_formed, "also-not-an-arn"]),
    );
    cases.push(Case {
        label: "the second destination of the rule".to_string(),
        method: "PUT",
        path,
        body,
        member_path: "configuration.rules.1.member.destinations.2.member.\
                      destinationTableBucketARN",
        requirement: format!("Member must satisfy regular expression pattern: {BUCKET_ARN_PATTERN}"),
        sent: vec!["also-not-an-arn".to_string()],
    });

    // A `@pattern` on a member of the nested structure itself: the role the
    // configuration names, long enough to clear the modelled minimum length and
    // still not an IAM role ARN.
    let malformed_role = "arn:aws:iam::000000000000:user/not-a-role";
    let (path, body) = replication_put(bucket, configuration(malformed_role, &[&well_formed]));
    cases.push(Case {
        label: "a role that is not an IAM role ARN".to_string(),
        method: "PUT",
        path,
        body,
        member_path: "configuration.role",
        requirement: format!("Member must satisfy regular expression pattern: {IAM_ROLE_PATTERN}"),
        sent: vec![malformed_role.to_string()],
    });

    // A `@range` minimum on a member two structures deep: the days a table
    // retains its records. `TableRecordExpirationSettings.days` targets
    // `PositiveInteger`, `@range(1, 2147483647)`, and this service words no
    // sentence of its own for it — so a retention no table can keep its records
    // for is the validator's refusal, whatever the number below the minimum and
    // whatever its width on the wire.
    for days in [0_i64, -1, -365, -4_294_967_296] {
        cases.push(Case {
            label: format!("a retention of {days} days"),
            method: "PUT",
            path: format!(
                "/table-record-expiration{}",
                query_string(&[("tableArn", table_arn.to_string())])
            ),
            body: Some(json!({
                "value": { "status": "enabled", "settings": { "days": days } },
            })),
            member_path: "value.settings.days",
            requirement: "Member must have value greater than or equal to 1".to_string(),
            sent: vec![days.to_string()],
        });
    }

    // An enumerated set, on the one member of this model three operations carry
    // the same nested structure for: the storage class a table bucket's tables
    // are stored in. The constraint belongs to the MEMBER, so the clause does not
    // move with the operation — a create that names the class, a create of a
    // table in that bucket, and the put that sets the bucket's default all state
    // the same member path and the same set, in the order this service states it
    // in ([`STORAGE_CLASS_SET`]).
    for class in REFUSED_STORAGE_CLASSES {
        cases.push(Case {
            label: format!("a bucket create naming the storage class {class:?}"),
            method: "PUT",
            path: "/buckets".to_string(),
            body: Some(json!({
                "name": "parameter-validation-class",
                "storageClassConfiguration": { "storageClass": class },
            })),
            member_path: "storageClassConfiguration.storageClass",
            requirement: enum_requirement(STORAGE_CLASS_SET),
            sent: vec![class.to_string()],
        });
        cases.push(Case {
            label: format!("a put of the storage class {class:?}"),
            method: "PUT",
            path: format!("/buckets/{}/storage-class", encode(bucket)),
            body: Some(json!({
                "storageClassConfiguration": { "storageClass": class },
            })),
            member_path: "storageClassConfiguration.storageClass",
            requirement: enum_requirement(STORAGE_CLASS_SET),
            sent: vec![class.to_string()],
        });
        cases.push(Case {
            label: format!("a table create naming the storage class {class:?}"),
            method: "PUT",
            path: format!("/tables/{}/resident", encode(bucket)),
            body: Some(json!({
                "name": "paramvalidation",
                "format": "ICEBERG",
                "storageClassConfiguration": { "storageClass": class },
            })),
            member_path: "storageClassConfiguration.storageClass",
            requirement: enum_requirement(STORAGE_CLASS_SET),
            sent: vec![class.to_string()],
        });
    }

    // The same put addressed to a bucket the store does not hold: the value is
    // refused for its shape before anything is looked up, so the clause is the
    // one above rather than the modelled not-found.
    cases.push(Case {
        label: "a put of a storage class on a bucket nothing created".to_string(),
        method: "PUT",
        path: format!(
            "/buckets/{}/storage-class",
            encode(&expected_bucket_arn(REGION, "parameter-validation-absent"))
        ),
        body: Some(json!({
            "storageClassConfiguration": { "storageClass": "ULTRA_FAST_TIER" },
        })),
        member_path: "storageClassConfiguration.storageClass",
        requirement: enum_requirement(STORAGE_CLASS_SET),
        sent: vec!["ULTRA_FAST_TIER".to_string()],
    });

    // A SECOND enumerated type, carried as a query member of a list rather than
    // inside a body, and declared in an order of its own as well: the type a
    // listing filters table buckets by.
    for bucket_type in REFUSED_BUCKET_TYPES {
        cases.push(Case {
            label: format!("a listing filtered by the bucket type {bucket_type:?}"),
            method: "GET",
            path: format!(
                "/buckets{}",
                query_string(&[("type", bucket_type.to_string())])
            ),
            body: None,
            member_path: "type",
            requirement: enum_requirement(TABLE_BUCKET_TYPE_SET),
            sent: vec![bucket_type.to_string()],
        });
    }

    // A `@length` bound at either end, on an input member of its own:
    // `VersionToken` is `@length(1, 2048)` and carries no pattern, so the empty
    // token is under the minimum and one character past 2048 is over the
    // maximum.
    let long_token = "t".repeat(2049);
    for (label, token, requirement) in [
        (
            "an empty version token",
            String::new(),
            "Member must have length greater than or equal to 1",
        ),
        (
            "a version token one character over the modelled maximum",
            long_token.clone(),
            "Member must have length less than or equal to 2048",
        ),
    ] {
        cases.push(Case {
            label: label.to_string(),
            method: "PUT",
            path: format!(
                "/table-bucket-replication{}",
                query_string(&[
                    ("tableBucketARN", bucket.to_string()),
                    ("versionToken", token.clone()),
                ])
            ),
            body: Some(json!({ "configuration": configuration(ROLE, &[&well_formed]) })),
            member_path: "versionToken",
            requirement: requirement.to_string(),
            sent: vec![token],
        });
    }

    cases
}

/// A table bucket, a namespace and one table AWS manages, for the rows that
/// address a bucket or a table. The refusals below happen in front of the
/// service, so what the store holds cannot change them — the resources exist so
/// that the accepted rows have something to be served against.
async fn fixture(emulator: &Emulator) -> (String, String) {
    let bucket = emulator
        .create_table_bucket(REGION, &json!({ "name": "parameter-validation" }))
        .await;
    assert_eq!(
        bucket.status, 200,
        "the fixture bucket must be created, got {status} {body}",
        status = bucket.status,
        body = bucket.body
    );
    let table_arn = create_table(emulator, REGION, "parameter-validation-tbl", "resident").await;
    mark_managed(emulator, &table_arn);
    (bucket.text("arn"), table_arn)
}

#[tokio::test]
async fn a_parameter_the_service_words_no_sentence_for_is_refused_by_the_validator() {
    let emulator = Emulator::new();
    let (bucket, table_arn) = fixture(&emulator).await;

    for case in cases(&bucket, &table_arn) {
        let refused = emulator
            .call(case.method, &case.path, REGION, case.body.as_ref())
            .await;
        assert_eq!(
            refused.status, 400,
            "case {label:?}: expected status 400, got {status} {body}",
            label = case.label,
            status = refused.status,
            body = refused.body
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(PARAMETER_VALIDATION),
            "case {label:?}: expected error type {PARAMETER_VALIDATION}, got {actual:?}",
            label = case.label,
            actual = refused.error_code
        );
        assert_eq!(
            refused.message(),
            case.expected_message(),
            "case {label:?}: expected the validator's envelope, got {actual:?}",
            label = case.label,
            actual = refused.message()
        );
        for value in &case.sent {
            // A one- or two-character value occurs inside ordinary words, and a
            // bound the clause states is a number the request may also carry.
            if value.chars().count() < 3 {
                continue;
            }
            assert!(
                !refused.message().contains(value.as_str()),
                "case {label:?}: the refusal must not echo the request's {value:?}, got {actual:?}",
                label = case.label,
                actual = refused.message()
            );
        }
    }
}

#[tokio::test]
async fn a_request_breaking_several_constraints_is_counted_in_the_plural() {
    let emulator = Emulator::new();
    let (bucket, _) = fixture(&emulator).await;

    // Two members of the same configuration, each broken: the role and the one
    // destination of its rule.
    let (path, body) = replication_put(
        &bucket,
        configuration("arn:aws:iam::000000000000:user/not-a-role", &["not-an-arn"]),
    );
    let refused = emulator.call("PUT", &path, REGION, body.as_ref()).await;
    assert_eq!(
        refused.status, 400,
        "expected status 400, got {status} {body}",
        status = refused.status,
        body = refused.body
    );
    assert_eq!(
        refused.error_code.as_deref(),
        Some(PARAMETER_VALIDATION),
        "expected error type {PARAMETER_VALIDATION}, got {actual:?}",
        actual = refused.error_code
    );
    let message = refused.message();
    assert!(
        message.starts_with("2 validation errors detected: "),
        "two broken constraints must be counted in the plural, got {message:?}"
    );
    for clause in [
        format!(
            "Value at 'configuration.role' failed to satisfy constraint: Member must satisfy \
             regular expression pattern: {IAM_ROLE_PATTERN}"
        ),
        format!(
            "Value at 'configuration.rules.1.member.destinations.1.member.\
             destinationTableBucketARN' failed to satisfy constraint: Member must satisfy regular \
             expression pattern: {BUCKET_ARN_PATTERN}"
        ),
    ] {
        assert!(
            message.contains(&clause),
            "the envelope must state {clause:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_value_at_every_published_bound_is_served() {
    let emulator = Emulator::new();
    let (bucket, table_arn) = fixture(&emulator).await;

    // `PositiveInteger` is `@range(1, 2147483647)`: both ends are retentions the
    // service stores, and the read reports them.
    for days in [1, 2_147_483_647] {
        let put = emulator
            .put_table_record_expiration_configuration(
                REGION,
                &table_arn,
                &json!({ "status": "enabled", "settings": { "days": days } }),
            )
            .await;
        assert_eq!(
            put.status, 204,
            "case {days}: a retention at a published bound must be served, got {status} {body}",
            status = put.status,
            body = put.body
        );
        let read = emulator
            .get_table_record_expiration_configuration(REGION, &table_arn)
            .await;
        let reported = read
            .body
            .get("configuration")
            .and_then(|configuration| configuration.get("settings"))
            .and_then(|settings| settings.get("days"))
            .and_then(Value::as_i64);
        assert_eq!(
            reported,
            Some(i64::from(days)),
            "case {days}: the read must report the stored retention, got {reported:?}"
        );
    }

    // `VersionToken` is `@length(1, 2048)`: a token of one character is inside
    // it, so the put is served or refused on its own terms — a precondition on a
    // configuration that is not there — rather than by the validator.
    let token_at_bound = emulator
        .put_table_bucket_replication(
            REGION,
            &[
                ("tableBucketARN", bucket.clone()),
                ("versionToken", "t".to_string()),
            ],
            &configuration(ROLE, &[&expected_bucket_arn(REGION, "replication-target")]),
        )
        .await;
    assert_ne!(
        token_at_bound.error_code.as_deref(),
        Some(PARAMETER_VALIDATION),
        "a token of one character is inside the modelled bound, got {body}",
        body = token_at_bound.body
    );

    // And a configuration whose every member satisfies its constraints is
    // served, under a token the read reports back.
    let served = emulator
        .put_table_bucket_replication(
            REGION,
            &[("tableBucketARN", bucket.clone())],
            &configuration(ROLE, &[&expected_bucket_arn(REGION, "replication-target")]),
        )
        .await;
    assert_eq!(
        served.status, 200,
        "a configuration inside every constraint must be served, got {status} {body}",
        status = served.status,
        body = served.body
    );
    let read = emulator.get_table_bucket_replication(REGION, &bucket).await;
    assert_eq!(
        read.text("versionToken"),
        served.text("versionToken"),
        "the read must report the configuration the put stored"
    );
}

/// The bracketed value set a requirement states, or `None` for a message
/// stating none.
fn stated_set(message: &str) -> Option<String> {
    let (_, set) = message.split_once("enum value set: ")?;
    Some(set.trim().to_string())
}

/// The values a bracketed set lists, in the order it lists them.
fn listed_values(value_set: &str) -> Vec<String> {
    value_set
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(", ")
        .map(str::to_string)
        .collect()
}

/// `declared` sorted — the order a refusal must state an enumerated type's
/// values in, derived from the model's declaration rather than restated.
fn sorted(declared: &[&str]) -> Vec<String> {
    let mut values: Vec<String> = declared.iter().map(|value| (*value).to_string()).collect();
    values.sort_unstable();
    values
}

#[tokio::test]
async fn an_enumerated_types_value_set_is_stated_sorted_whatever_carried_it() {
    let emulator = Emulator::new();
    let (bucket, table_arn) = fixture(&emulator).await;

    // The set each refusal states is read off the SERVICE's own message and then
    // held to the sorted order of the values the model declares — so what is
    // pinned is the order and the completeness of the set rather than one
    // literal. Every row carrying one type states the SAME set, however the value
    // was spelled and whichever operation, member depth or addressed resource
    // carried it.
    for case in cases(&bucket, &table_arn) {
        let Some(expected_set) = [
            (STORAGE_CLASS_SET, &STORAGE_CLASS_DECLARED),
            (TABLE_BUCKET_TYPE_SET, &TABLE_BUCKET_TYPE_DECLARED),
        ]
        .into_iter()
        .find(|(set, _)| case.requirement.ends_with(set))
        .map(|(_, declared)| sorted(declared)) else {
            // A row about another constraint class: the orders below are only
            // about an enumerated type's value set.
            continue;
        };

        let refused = emulator
            .call(case.method, &case.path, REGION, case.body.as_ref())
            .await;
        let message = refused.message();
        let stated = stated_set(&message).unwrap_or_else(|| {
            panic!(
                "case {label:?}: the message states no enum value set, got {message:?}",
                label = case.label
            )
        });
        let listed = listed_values(&stated);
        assert_eq!(
            listed, expected_set,
            "case {label:?}: the clause must list every declared value sorted \
             ({expected_set:?}), got {listed:?}",
            label = case.label
        );
    }
}

#[tokio::test]
async fn a_refused_class_changes_nothing_and_every_admitted_class_is_served() {
    let emulator = Emulator::new();
    let (bucket, _) = fixture(&emulator).await;

    /// The storage class `GetTableBucketStorageClass` reports for `arn`.
    async fn reported(emulator: &Emulator, arn: &str, case: &str) -> String {
        let read = emulator.get_table_bucket_storage_class(REGION, arn).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetTableBucketStorageClass must succeed, got {status} {body}",
            status = read.status,
            body = read.body
        );
        read.body
            .get("storageClassConfiguration")
            .and_then(|configuration| configuration.get("storageClass"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the read must carry \
                     storageClassConfiguration.storageClass, got {body}",
                    body = read.body
                )
            })
            .to_string()
    }

    // A refused put has no effect: the class the bucket reports after it is the
    // one it reported before.
    for class in REFUSED_STORAGE_CLASSES {
        let before = reported(&emulator, &bucket, class).await;
        let refused = emulator
            .put_table_bucket_storage_class(
                REGION,
                &bucket,
                &json!({ "storageClass": class }),
            )
            .await;
        assert_eq!(
            refused.status, 400,
            "case {class:?}: expected status 400, got {status} {body}",
            status = refused.status,
            body = refused.body
        );
        let after = reported(&emulator, &bucket, class).await;
        assert_eq!(
            after, before,
            "case {class:?}: a refused put must leave the reported class at {before:?}, got \
             {after:?}"
        );
    }

    // And every value the type DOES admit is served and read back, so what the
    // rows above refuse is the values outside the type and not the member.
    for class in STORAGE_CLASS_DECLARED {
        let put = emulator
            .put_table_bucket_storage_class(
                REGION,
                &bucket,
                &json!({ "storageClass": class }),
            )
            .await;
        assert_eq!(
            put.status, 200,
            "case {class:?}: PutTableBucketStorageClass must succeed, got {status} {body}",
            status = put.status,
            body = put.body
        );
        let read = reported(&emulator, &bucket, class).await;
        assert_eq!(
            read, class,
            "case {class:?}: the read must report the class the put named, got {read:?}"
        );
    }
}
