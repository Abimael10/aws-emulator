// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS a request whose values break the constraints its model declares,
//! over the wire.
//!
//! RULE: a request a modelled constraint refuses is answered HTTP 400 with `Error.Code`
//! `ValidationException` and an `Error.Message` of the form
//! `<N> validation error(s) detected: ` followed by one clause per violated member,
//! separated by `; `, where
//!
//! * the envelope's only variable part is `N`, the number of violated members — the
//!   parenthesised `(s)` is fixed text and is emitted for one violation as well as for
//!   many;
//! * a clause is `Value at '<member path>' failed to satisfy constraint: <requirement>`,
//!   naming the member the request carried the value at and the requirement it broke, and
//!   stating NO value — not the one the request submitted, and not a `null` for a member
//!   that was absent;
//! * the requirement for a `@pattern` quotes the expression WITHOUT the `^`/`$` anchors the
//!   model writes it with, while the check itself still uses the model's pattern as written
//!   (a value the unanchored body would admit mid-string is still refused).
//!
//! The rows span what the rule generalises over: the constraint CLASS (`@pattern`, `@range`
//! at each bound, `@length` at each bound on a string and on a list, and an absent
//! `@required` member), the member's SHAPE (string, integer, list), whether the member is
//! `@sensitive` (so the framework withheld its value anyway) or not (so dropping the value
//! is this service's own doing), the OPERATION the member belongs to (a create and an
//! update), and the NUMBER of violated members in one request.
//!
//! Every constraint named here comes from
//! `smithy_query(query="operation", operation="CreateDbInstance,UpdateDbInstance")`.

use serde_json::{json, Value};

use crate::db_instance_lifecycle::{call, ok, router};

/// The envelope's fixed text, after the count.
const ENVELOPE_SUFFIX: &str = " validation error(s) detected: ";

/// The minimum a `CreateDbInstance` request must carry, per the model's `@required`
/// members (`name`, `password`, `dbInstanceType`, `vpcSubnetIds`, `vpcSecurityGroupIds`,
/// `allocatedStorage`).
fn minimal_create(name: &str) -> Value {
    json!({
        "name": name,
        "password": "supersecret1",
        "dbInstanceType": "db.influx.medium",
        "allocatedStorage": 20,
        "vpcSubnetIds": ["subnet-aaa"],
        "vpcSecurityGroupIds": ["sg-aaa"],
    })
}

/// The `Code` and `Message` of an `awsJson1_0` error answer.
fn refusal(case: &str, status: u16, body: &Value) -> (String, String) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {body}"
    );
    let code = body["__type"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a `__type`, got {body}"))
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string();
    let message = body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_else(|| panic!("case {case:?}: expected a message, got {body}"))
        .to_string();
    (code, message)
}

/// One row: a `CreateDbInstance` body and the whole message the refusal must carry.
struct CreateRow {
    case: &'static str,
    body: Value,
    message: &'static str,
}

/// Rows spanning the constraint classes, member shapes and bounds the rule holds over.
fn create_rows() -> Vec<CreateRow> {
    let mut long_name = String::from("a");
    long_name.push_str(&"b".repeat(40)); // DbInstanceName length 3..40 — one past the max.
    vec![
        CreateRow {
            case: "the @pattern of a @sensitive string member (password ^[a-zA-Z0-9]+$)",
            body: {
                let mut body = minimal_create("pattern-password");
                body["password"] = json!("bad-pass!1");
                body
            },
            message: "1 validation error(s) detected: Value at 'password' failed to satisfy \
                      constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9]+",
        },
        CreateRow {
            case: "the @pattern of a string member that is not @sensitive \
                   (dbParameterGroupIdentifier ^[a-zA-Z0-9]+$)",
            body: {
                let mut body = minimal_create("pattern-group-id");
                body["dbParameterGroupIdentifier"] = json!("not-a-valid-id");
                body
            },
            message: "1 validation error(s) detected: Value at 'dbParameterGroupIdentifier' \
                      failed to satisfy constraint: Member must satisfy regular expression \
                      pattern: [a-zA-Z0-9]+",
        },
        CreateRow {
            case: "a @pattern whose body is a character class, not an alternation \
                   (bucket ^[^_\"][^\"]*$)",
            body: {
                let mut body = minimal_create("pattern-bucket");
                body["bucket"] = json!("_leading-underscore");
                body
            },
            message: "1 validation error(s) detected: Value at 'bucket' failed to satisfy \
                      constraint: Member must satisfy regular expression pattern: \
                      [^_\"][^\"]*",
        },
        CreateRow {
            case: "a @pattern carrying a group (name \
                   ^[a-zA-Z][a-zA-Z0-9]*(-[a-zA-Z0-9]+)*$)",
            body: minimal_create("has_underscore"),
            message: "1 validation error(s) detected: Value at 'name' failed to satisfy \
                      constraint: Member must satisfy regular expression pattern: \
                      [a-zA-Z][a-zA-Z0-9]*(-[a-zA-Z0-9]+)*",
        },
        CreateRow {
            case: "the @range minimum of an integer member (allocatedStorage min 20)",
            body: {
                let mut body = minimal_create("range-storage-low");
                body["allocatedStorage"] = json!(19);
                body
            },
            message: "1 validation error(s) detected: Value at 'allocatedStorage' failed to \
                      satisfy constraint: Member must have value greater than or equal to 20",
        },
        CreateRow {
            case: "the @range maximum of an integer member (allocatedStorage max 15360)",
            body: {
                let mut body = minimal_create("range-storage-high");
                body["allocatedStorage"] = json!(15361);
                body
            },
            message: "1 validation error(s) detected: Value at 'allocatedStorage' failed to \
                      satisfy constraint: Member must have value less than or equal to 15360",
        },
        CreateRow {
            case: "the @range minimum of another integer member (port min 1024)",
            body: {
                let mut body = minimal_create("range-port-low");
                body["port"] = json!(1023);
                body
            },
            message: "1 validation error(s) detected: Value at 'port' failed to satisfy \
                      constraint: Member must have value greater than or equal to 1024",
        },
        CreateRow {
            case: "the @length minimum of a string member (name min 3)",
            body: minimal_create("ab"),
            message: "1 validation error(s) detected: Value at 'name' failed to satisfy \
                      constraint: Member must have length greater than or equal to 3",
        },
        CreateRow {
            case: "the @length maximum of a string member (name max 40)",
            body: minimal_create(&long_name),
            message: "1 validation error(s) detected: Value at 'name' failed to satisfy \
                      constraint: Member must have length less than or equal to 40",
        },
        CreateRow {
            case: "the @length maximum of a LIST member (vpcSubnetIds max 6)",
            body: {
                let mut body = minimal_create("too-many-subnets");
                body["vpcSubnetIds"] = json!([
                    "subnet-a1", "subnet-a2", "subnet-a3",
                    "subnet-a4", "subnet-a5", "subnet-a6", "subnet-a7"
                ]);
                body
            },
            message: "1 validation error(s) detected: Value at 'vpcSubnetIds' failed to \
                      satisfy constraint: Member must have length less than or equal to 6",
        },
        CreateRow {
            case: "the @length minimum of a LIST member (vpcSecurityGroupIds min 1)",
            body: {
                let mut body = minimal_create("no-groups");
                body["vpcSecurityGroupIds"] = json!([]);
                body
            },
            message: "1 validation error(s) detected: Value at 'vpcSecurityGroupIds' failed \
                      to satisfy constraint: Member must have length greater than or equal \
                      to 1",
        },
        CreateRow {
            case: "an absent @required member (dbInstanceType), whose clause states no \
                   `null` either",
            body: {
                let mut body = minimal_create("no-instance-type");
                body.as_object_mut()
                    .expect("the request body is an object")
                    .remove("dbInstanceType");
                body
            },
            message: "1 validation error(s) detected: Value at 'dbInstanceType' failed to \
                      satisfy constraint: Member must not be null",
        },
    ]
}

#[tokio::test]
async fn a_create_refusal_states_the_count_envelope_and_a_valueless_clause() {
    let router = router();
    for row in create_rows() {
        let case = row.case;
        let (status, body) = call(&router, "CreateDbInstance", row.body).await;
        let (code, message) = refusal(case, status, &body);
        assert_eq!(
            code, "ValidationException",
            "case {case:?}: expected the modelled error code, got {code:?}"
        );
        assert_eq!(
            message, row.message,
            "case {case:?}: expected {:?}, got {message:?}",
            row.message
        );
    }
}

#[tokio::test]
async fn an_update_refusal_is_worded_the_same_way_as_a_create_refusal() {
    let router = router();
    let created = ok(
        "an instance to update",
        call(&router, "CreateDbInstance", minimal_create("update-wording")).await,
    );
    let id = created["id"]
        .as_str()
        .expect("the create reports the minted id")
        .to_string();

    // Rows: the recorded shrink below the storage floor, the ceiling of the same member,
    // the other integer member's floor, and a @pattern on the update's own identifier.
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            "a storage shrink below the modelled minimum 20",
            json!({ "identifier": &id, "allocatedStorage": 10 }),
            "1 validation error(s) detected: Value at 'allocatedStorage' failed to satisfy \
             constraint: Member must have value greater than or equal to 20",
        ),
        (
            "a storage grow past the modelled maximum 15360",
            json!({ "identifier": &id, "allocatedStorage": 20_000 }),
            "1 validation error(s) detected: Value at 'allocatedStorage' failed to satisfy \
             constraint: Member must have value less than or equal to 15360",
        ),
        (
            "a port below the modelled minimum 1024",
            json!({ "identifier": &id, "port": 80 }),
            "1 validation error(s) detected: Value at 'port' failed to satisfy constraint: \
             Member must have value greater than or equal to 1024",
        ),
        (
            "an identifier outside the modelled pattern ^[a-zA-Z0-9]+$",
            json!({ "identifier": "not-an-id", "port": 9000 }),
            "1 validation error(s) detected: Value at 'identifier' failed to satisfy \
             constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9]+",
        ),
    ];

    for (case, body, expected) in rows {
        let (status, answer) = call(&router, "UpdateDbInstance", body).await;
        let (code, message) = refusal(case, status, &answer);
        assert_eq!(
            code, "ValidationException",
            "case {case:?}: expected the modelled error code, got {code:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn the_count_in_the_envelope_is_the_number_of_violated_members() {
    let router = router();
    // Two members broken in one request: a @range floor and a @pattern.
    let mut body = minimal_create("two_bad_members");
    body["allocatedStorage"] = json!(19);
    let case = "two violated members in one request";
    let (status, answer) = call(&router, "CreateDbInstance", body).await;
    let (code, message) = refusal(case, status, &answer);
    assert_eq!(
        code, "ValidationException",
        "case {case:?}: expected the modelled error code, got {code:?}"
    );
    assert!(
        message.starts_with(&format!("2{ENVELOPE_SUFFIX}")),
        "case {case:?}: expected the envelope to count two errors, got {message:?}"
    );
    for clause in [
        "Value at 'name' failed to satisfy constraint: Member must satisfy regular \
         expression pattern: [a-zA-Z][a-zA-Z0-9]*(-[a-zA-Z0-9]+)*",
        "Value at 'allocatedStorage' failed to satisfy constraint: Member must have value \
         greater than or equal to 20",
    ] {
        assert!(
            message.contains(clause),
            "case {case:?}: expected the message to carry {clause:?}, got {message:?}"
        );
    }
    assert!(
        message.contains("; "),
        "case {case:?}: expected two clauses separated by '; ', got {message:?}"
    );
    assert!(
        !message.contains("19") && !message.contains("two_bad_members"),
        "case {case:?}: no clause may echo a submitted value, got {message:?}"
    );
}

#[tokio::test]
async fn the_anchors_are_dropped_from_the_message_but_not_from_the_check() {
    let router = router();
    // `password` must match `^[a-zA-Z0-9]+$` WHOLE. A value whose alphanumeric run is only
    // a prefix satisfies the unanchored body the message quotes, and is still refused —
    // so the rendering is a rendering and not the rule that was applied.
    for (case, password) in [
        ("an alphanumeric prefix then a refused character", "abcdefgh!"),
        ("a refused character then an alphanumeric run", "!abcdefgh"),
        ("a refused character in the middle", "abcd!efgh"),
    ] {
        let mut body = minimal_create("anchored-check");
        body["password"] = json!(password);
        let (status, answer) = call(&router, "CreateDbInstance", body).await;
        let (_code, message) = refusal(case, status, &answer);
        assert_eq!(
            message,
            "1 validation error(s) detected: Value at 'password' failed to satisfy \
             constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9]+",
            "case {case:?}: got {message:?}"
        );
    }
    // And a value the model's pattern DOES admit is accepted, so the check was not loosened.
    let mut body = minimal_create("anchored-ok");
    body["password"] = json!("abcdefgh");
    ok(
        "a password the model's pattern admits",
        call(&router, "CreateDbInstance", body).await,
    );
}
