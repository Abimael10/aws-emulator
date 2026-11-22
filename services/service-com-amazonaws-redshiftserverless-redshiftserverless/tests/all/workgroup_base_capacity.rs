// SPDX-License-Identifier: CC0-1.0

//! The base capacity a workgroup may be given, over this service's own `awsJson1_1` wire.
//!
//! RULE: a `baseCapacity` BELOW the smallest standard RPU size is in the tiny RPU range, where
//! the service serves exactly two sizes — so any other size in that range is refused with the
//! sentence it words for that class, and a size at or above the smallest standard size is
//! served. `baseCapacity`'s valid sizes are documented on the member rather than modelled as a
//! constraint, so the boundary validator cannot refuse one and the handler is what does.
//!
//! The rows span the class the rule generalises over: each size strictly inside the tiny range
//! (both the two the service serves and every one it does not), the smallest standard size,
//! which is the first size past the range, and a larger standard size. Both calls that take the
//! member are held to it — `CreateWorkgroup`, which decides the size a workgroup comes up with,
//! and `UpdateWorkgroup`, which re-values it on a workgroup that exists.

use serde_json::{json, Value};

use crate::namespace_workgroup_lifecycle::{call, emulator, error_code};

/// The error this service answers a base capacity it does not serve with, and the status it
/// carries (`ValidationException`'s own `@httpError`).
const VALIDATION_EXCEPTION: &str = "ValidationException";
const REFUSAL_STATUS: u16 = 400;

/// The sentence this service words for a size in the tiny RPU range it does not serve, which
/// names the two it does.
const TINY_RANGE_SENTENCE: &str = "The base capacity must be 2 or 4 in tiny RPU range.";

/// The smallest STANDARD base capacity, which is the first size past the tiny RPU range.
const SMALLEST_STANDARD: i64 = 8;

/// One size the rule decides about: what it is, and whether this service serves it.
struct Row {
    case: &'static str,
    base_capacity: i64,
    served: bool,
}

/// Every size strictly inside the tiny RPU range, the first size past it and a larger standard
/// one — so the rows differ along which side of the range's two served sizes the value falls on
/// as well as along the range boundary itself.
const ROWS: &[Row] = &[
    Row {
        case: "one RPU: inside the tiny range and not one of the two sizes served there",
        base_capacity: 1,
        served: false,
    },
    Row {
        case: "two RPUs: the smaller of the two tiny sizes served",
        base_capacity: 2,
        served: true,
    },
    Row {
        case: "three RPUs: between the two tiny sizes served",
        base_capacity: 3,
        served: false,
    },
    Row {
        case: "four RPUs: the larger of the two tiny sizes served",
        base_capacity: 4,
        served: true,
    },
    Row {
        case: "five RPUs: above the larger tiny size, still inside the range",
        base_capacity: 5,
        served: false,
    },
    Row {
        case: "six RPUs: still inside the range",
        base_capacity: 6,
        served: false,
    },
    Row {
        case: "seven RPUs: the last size inside the range",
        base_capacity: SMALLEST_STANDARD - 1,
        served: false,
    },
    Row {
        case: "eight RPUs: the smallest standard size, the first past the range",
        base_capacity: SMALLEST_STANDARD,
        served: true,
    },
    Row {
        case: "a larger standard size",
        base_capacity: SMALLEST_STANDARD * 16,
        served: true,
    },
];

/// A name of the form both `NamespaceName` and `WorkgroupName` admit
/// (`@length(min: 3, max: 64)`, `^[a-z0-9-]+$`), keyed by the row's own size so two rows never
/// collide.
fn named(prefix: &str, base_capacity: i64) -> String {
    format!("{prefix}-rpu-{base_capacity}")
}

/// The namespace a row's workgroup is the compute of, created first because a workgroup names
/// one.
async fn namespace(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    name: &str,
) {
    let (status, body) = call(router, "CreateNamespace", json!({ "namespaceName": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the namespace {name:?} to be created, got {status} with {body}"
    );
}

/// The message out of one awsJson error envelope.
fn message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn create_serves_only_the_base_capacities_the_service_publishes() {
    for row in ROWS {
        let router = emulator();
        let case = row.case;
        let namespace_name = named("bc-ns", row.base_capacity);
        let workgroup_name = named("bc-wg", row.base_capacity);
        namespace(&router, case, &namespace_name).await;

        let (status, body) = call(
            &router,
            "CreateWorkgroup",
            json!({
                "namespaceName": namespace_name,
                "workgroupName": workgroup_name,
                "baseCapacity": row.base_capacity,
            }),
        )
        .await;

        if row.served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected a size this service serves to be accepted, got {status} \
                 with {body}"
            );
            assert_eq!(
                body.get("workgroup")
                    .and_then(|workgroup| workgroup.get("baseCapacity"))
                    .and_then(Value::as_i64),
                Some(row.base_capacity),
                "case {case:?}: expected the workgroup to come up at the size asked for, got \
                 {body}"
            );
            // And the size is the one a later read reports, so the create wrote it rather than
            // only answering it.
            let (status, read) = call(
                &router,
                "GetWorkgroup",
                json!({ "workgroupName": workgroup_name }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the created workgroup to be readable, got {status} with \
                 {read}"
            );
            assert_eq!(
                read.get("workgroup")
                    .and_then(|workgroup| workgroup.get("baseCapacity"))
                    .and_then(Value::as_i64),
                Some(row.base_capacity),
                "case {case:?}: expected the stored size to be the one asked for, got {read}"
            );
            continue;
        }

        assert_eq!(
            (status, error_code(&body).as_str()),
            (REFUSAL_STATUS, VALIDATION_EXCEPTION),
            "case {case:?}: expected a size this service does not serve to be refused, got \
             {status} with {body}"
        );
        assert_eq!(
            message(&body),
            TINY_RANGE_SENTENCE,
            "case {case:?}: expected this service's own sentence for a size in the tiny RPU \
             range, got {body}"
        );
        // Nothing was created: a refused create leaves no workgroup behind.
        let (status, read) = call(
            &router,
            "GetWorkgroup",
            json!({ "workgroupName": workgroup_name }),
        )
        .await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case:?}: expected no workgroup for a refused create, got {status} with {read}"
        );
    }
}

#[tokio::test]
async fn update_serves_only_the_base_capacities_the_service_publishes() {
    for row in ROWS {
        let router = emulator();
        let case = row.case;
        let namespace_name = named("bu-ns", row.base_capacity);
        let workgroup_name = named("bu-wg", row.base_capacity);
        namespace(&router, case, &namespace_name).await;

        // The workgroup starts at the smallest standard size, which every row's update then
        // asks to re-value.
        let (status, body) = call(
            &router,
            "CreateWorkgroup",
            json!({
                "namespaceName": namespace_name,
                "workgroupName": workgroup_name,
                "baseCapacity": SMALLEST_STANDARD,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the workgroup to be created, got {status} with {body}"
        );

        let (status, body) = call(
            &router,
            "UpdateWorkgroup",
            json!({
                "workgroupName": workgroup_name,
                "baseCapacity": row.base_capacity,
            }),
        )
        .await;

        if row.served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected a size this service serves to be accepted, got {status} \
                 with {body}"
            );
        } else {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (REFUSAL_STATUS, VALIDATION_EXCEPTION),
                "case {case:?}: expected a size this service does not serve to be refused, got \
                 {status} with {body}"
            );
            assert_eq!(
                message(&body),
                TINY_RANGE_SENTENCE,
                "case {case:?}: expected this service's own sentence for a size in the tiny RPU \
                 range, got {body}"
            );
        }

        // Either way the workgroup carries the size the service accepted: the one the update
        // asked for where it serves it, and the one it was created with where it does not.
        let expected = match row.served {
            true => row.base_capacity,
            false => SMALLEST_STANDARD,
        };
        let (status, read) = call(
            &router,
            "GetWorkgroup",
            json!({ "workgroupName": workgroup_name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the workgroup to be readable, got {status} with {read}"
        );
        assert_eq!(
            read.get("workgroup")
                .and_then(|workgroup| workgroup.get("baseCapacity"))
                .and_then(Value::as_i64),
            Some(expected),
            "case {case:?}: expected the stored size to be {expected}, got {read}"
        );
    }
}
