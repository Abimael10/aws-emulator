// SPDX-License-Identifier: CC0-1.0

//! How IAM states a refusal of a value outside an enumerated set when that value arrived
//! INSIDE one of an operation's collections, end to end over the awsQuery wire.
//!
//! The rules under test:
//!
//! 1. the clause for a value outside its member's enum names the member path the value
//!    arrived at and the admissible set, and ECHOES THE VALUE where the value was reached
//!    inside one of the input's collections — `1 validation error detected: Value '<the
//!    value>' at '<the path>' failed to satisfy constraint: Member must satisfy enum value
//!    set: [<the admissible values>]`. That is the split the `@pattern` class is already
//!    stated under (`tests/all/nested_member_clause_wording.rs`): the value-free restating is
//!    for a member of the input ITSELF, which `tests/all/enum_refusal_wording.rs` owns;
//! 2. the set is stated in the order IAM publishes for that enum — a property of the ENUM,
//!    so it is the same set in the same order wherever the value sat, whichever operation
//!    carried it;
//! 3. the refusal is reached before the simulation runs, so a request whose entry names a
//!    value the set DOES declare is served: the clause is not a blanket refusal of the
//!    member.
//!
//! The rows differ along what the rules generalise over: which operation carries the
//! collection (both simulations), which position in the list the value arrived at, the FORM
//! of the rejected value (a name no enum of this service declares, an admissible value in the
//! wrong case, an admissible list type in the wrong case, an empty value) and what the
//! entries around it carried. No single request is the specification.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns, and the envelope, the
//! code and the message are asserted with the `RefusalCase` table
//! `tests/all/validation_error_clauses.rs` owns, so an input refusal is read exactly one way
//! across this crate.

use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::
    CONTEXT_KEY_TYPE_ENUM_VALUES;

use crate::enum_refusal_wording::{expected_message, KEY_ENCODING};
use crate::role_lifecycle::param;
use crate::user_lifecycle::{account_id, call, element, error_code, router};
use crate::validation_error_clauses::{assert_refused, one_error, RefusalCase};

/// An identity policy with a single statement, so the custom simulation's own members are
/// inside their published bounds and the enum is the only thing a row breaks.
const POLICY_DOCUMENT: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}]}"#;

/// An action name inside `ActionNameType`'s published bounds (length 3..128) that names a
/// service and an action, as `ActionNames` documents ("each operation must include the
/// service identifier, such as `iam:CreateUser`").
const ACTION_NAME: &str = "iam:ListUsers";

/// A condition context key name inside `ContextKeyNameType`'s published bounds (length
/// 5..256), with the service prefix the member documents.
const CONTEXT_KEY_NAME: &str = "aws:username";

/// `iam#ContextKeyTypeEnum` in the order IAM states the set in when it refuses a value
/// outside it — neither the order the model declares the values in (`string, stringList,
/// numeric, numericList, boolean, booleanList, ip, ipList, binary, binaryList, date,
/// dateList`) nor their sorted order. The order belongs to the enum, so every member
/// targeting it is stated this way; the test below reads the set back OFF THE WIRE and
/// compares it, as a SET, with the values the model declares, so an order can neither drop a
/// value the enum declares nor name one it does not.
const PUBLISHED_CONTEXT_KEY_TYPES: &[&str] = &[
    "binary",
    "ipList",
    "booleanList",
    "ip",
    "boolean",
    "numericList",
    "dateList",
    "numeric",
    "date",
    "stringList",
    "string",
    "binaryList",
];

/// The clause for a value reached inside one of the input's collections that was outside its
/// member's enum: the value it carried, the member path it arrived at, and the admissible set
/// in the published order.
fn nested_enum_clause(value: &str, member_path: &str, published: &[&str]) -> String {
    one_error(&format!(
        "Value '{value}' at '{member_path}' failed to satisfy constraint: Member must satisfy \
         enum value set: [{}]",
        published.join(", ")
    ))
}

/// The `ContextEntries.member.<position>.*` parameters for one entry of a simulation's
/// context, written out as the query protocol lays a list of structures out.
fn context_entry(position: usize, key_type: &str) -> Vec<(String, String)> {
    vec![
        param(
            &format!("ContextEntries.member.{position}.ContextKeyName"),
            CONTEXT_KEY_NAME,
        ),
        param(
            &format!("ContextEntries.member.{position}.ContextKeyValues.member.1"),
            "a-value",
        ),
        param(
            &format!("ContextEntries.member.{position}.ContextKeyType"),
            key_type,
        ),
    ]
}

/// The parameters of a `SimulateCustomPolicy` call carrying `entries` as its context.
fn custom_policy_params(entries: Vec<Vec<(String, String)>>) -> Vec<(String, String)> {
    let mut params = vec![
        param("PolicyInputList.member.1", POLICY_DOCUMENT),
        param("ActionNames.member.1", ACTION_NAME),
    ];
    params.extend(entries.into_iter().flatten());
    params
}

/// The parameters of a `SimulatePrincipalPolicy` call carrying `entries` as its context. The
/// principal is named by an ARN the account holds no role under, because the refusal under
/// test is stated before anything is looked up.
fn principal_policy_params(entries: Vec<Vec<(String, String)>>) -> Vec<(String, String)> {
    let mut params = vec![
        param(
            "PolicySourceArn",
            &format!("arn:aws:iam::{}:role/a-role-not-held", account_id()),
        ),
        param("ActionNames.member.1", ACTION_NAME),
    ];
    params.extend(entries.into_iter().flatten());
    params
}

/// One row: the case, the call to make, the member path the clause must name, and the value
/// the clause must echo.
struct NestedEnumRow {
    case: &'static str,
    action: &'static str,
    params: Vec<(String, String)>,
    member_path: &'static str,
    value: &'static str,
}

/// The rows: both simulations, the first and a later position in the list, and four forms of
/// a value outside the set.
fn rows() -> Vec<NestedEnumRow> {
    vec![
        NestedEnumRow {
            case: "an admissible type in the wrong case, on the custom simulation",
            action: "SimulateCustomPolicy",
            params: custom_policy_params(vec![context_entry(1, "String")]),
            member_path: "contextEntries.1.member.contextKeyType",
            value: "String",
        },
        NestedEnumRow {
            case: "an admissible type in the wrong case, on the principal simulation",
            action: "SimulatePrincipalPolicy",
            params: principal_policy_params(vec![context_entry(1, "String")]),
            member_path: "contextEntries.1.member.contextKeyType",
            value: "String",
        },
        NestedEnumRow {
            case: "a type name no enum of this service declares, at the second position",
            action: "SimulateCustomPolicy",
            params: custom_policy_params(vec![
                context_entry(1, "string"),
                context_entry(2, "text"),
            ]),
            member_path: "contextEntries.2.member.contextKeyType",
            value: "text",
        },
        NestedEnumRow {
            case: "an admissible list type in the wrong case, at the third position",
            action: "SimulatePrincipalPolicy",
            params: principal_policy_params(vec![
                context_entry(1, "string"),
                context_entry(2, "numeric"),
                context_entry(3, "StringList"),
            ]),
            member_path: "contextEntries.3.member.contextKeyType",
            value: "StringList",
        },
        NestedEnumRow {
            case: "an empty type on the custom simulation",
            action: "SimulateCustomPolicy",
            params: custom_policy_params(vec![context_entry(1, "")]),
            member_path: "contextEntries.1.member.contextKeyType",
            value: "",
        },
    ]
}

/// RULES 1 and 2: every row is refused 400 under the one input-refusal code with the clause
/// naming the value it carried, the position it arrived at, and the admissible set in the
/// published order.
#[tokio::test]
async fn a_nested_value_outside_an_enum_set_is_stated_with_the_value_it_carried() {
    let router = router();

    for NestedEnumRow {
        case,
        action,
        params,
        member_path,
        value,
    } in rows()
    {
        assert_refused(
            &router,
            RefusalCase {
                case: case.to_string(),
                action,
                params,
                expected: nested_enum_clause(value, member_path, PUBLISHED_CONTEXT_KEY_TYPES),
                rejected: None,
            },
        )
        .await;
    }
}

/// RULE 2: the set a refusal states ON THE WIRE is the set the model declares — the same
/// values, so a clause can neither drop one the enum declares nor name one it does not —
/// written in the order this service publishes for that enum, whichever operation and
/// position the value arrived at.
#[tokio::test]
async fn each_stated_set_is_the_modelled_set_in_the_published_order() {
    let router = router();

    for NestedEnumRow {
        case,
        action,
        params,
        ..
    } in rows()
    {
        let (status, refused) = call(
            &router,
            action,
            &params
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect::<Vec<(&str, &str)>>(),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "ValidationError".to_string()),
            "case {case:?}: expected the refusal of a value outside the set, got \
             {status}: {refused}"
        );
        let stated = stated_set(case, &element(&refused, "Message"));
        let mut as_a_set = stated.clone();
        as_a_set.sort();
        let mut modelled: Vec<String> = CONTEXT_KEY_TYPE_ENUM_VALUES
            .iter()
            .map(|value| (*value).to_string())
            .collect();
        modelled.sort();
        assert_eq!(
            as_a_set, modelled,
            "case {case:?}: expected the stated set to be the modelled values {modelled:?}, \
             got {stated:?}"
        );
        let published: Vec<String> = PUBLISHED_CONTEXT_KEY_TYPES
            .iter()
            .map(|value| (*value).to_string())
            .collect();
        assert_eq!(
            stated, published,
            "case {case:?}: expected the published order {published:?}, got {stated:?}"
        );
    }
}

/// The values the bracketed set of `message` states, in the order it states them.
fn stated_set(case: &str, message: &str) -> Vec<String> {
    let (_, bracketed) = message
        .split_once("Member must satisfy enum value set: [")
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a stated enum value set, got {message:?}")
        });
    let listed = bracketed.strip_suffix(']').unwrap_or_else(|| {
        panic!("case {case:?}: expected the stated set to be bracketed, got {message:?}")
    });
    listed.split(", ").map(str::to_string).collect()
}

/// RULE 1's split: the SAME constraint class is stated two ways — with the value where it was
/// reached inside one of the input's collections, and without it where it was supplied for a
/// member of the input itself.
#[tokio::test]
async fn the_clause_states_the_value_only_where_it_was_reached_inside_a_collection() {
    let router = router();

    // Reached inside a collection: the value it carried, at the position it arrived at.
    let nested = custom_policy_params(vec![context_entry(1, "Numeric")]);
    let (status, refused) = call(
        &router,
        "SimulateCustomPolicy",
        &nested
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect::<Vec<(&str, &str)>>(),
    )
    .await;
    assert_eq!(
        status, 400,
        "a nested type outside the set must be refused, got {status}: {refused}"
    );
    let message = element(&refused, "Message");
    assert_eq!(
        message,
        nested_enum_clause(
            "Numeric",
            "contextEntries.1.member.contextKeyType",
            PUBLISHED_CONTEXT_KEY_TYPES
        ),
        "expected a nested clause to name the value at its position, got {message:?}"
    );

    // Supplied for a member of the input ITSELF: the member and the set, and nothing of the
    // value. The clause is the one `tests/all/enum_refusal_wording.rs` composes, so it is
    // spelled in one place.
    let (status, refused) = call(
        &router,
        "GetSSHPublicKey",
        &[
            ("UserName", "a-user"),
            ("SSHPublicKeyId", "APKAEIBAERJR2EXAMPLE"),
            ("Encoding", "OpenSSH"),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "an encoding outside the set must be refused, got {status}: {refused}"
    );
    let message = element(&refused, "Message");
    assert_eq!(
        message,
        expected_message("encoding", KEY_ENCODING),
        "expected an input's own member to be stated without its value, got {message:?}"
    );
    assert!(
        !refused.contains("OpenSSH"),
        "expected the clause for an input's own member to echo nothing of the value, got \
         {refused}"
    );
}

/// RULE 3: the refusal is about the value being outside the set, so an entry naming a value
/// the set DOES declare is taken and the simulation is served.
#[tokio::test]
async fn an_entry_naming_an_admissible_type_is_served() {
    let router = router();

    for key_type in ["string", "stringList", "numeric", "boolean", "ip", "date"] {
        let params = custom_policy_params(vec![context_entry(1, key_type)]);
        let (status, body) = call(
            &router,
            "SimulateCustomPolicy",
            &params
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect::<Vec<(&str, &str)>>(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {key_type:?}: an admissible context key type must be taken, got \
             {status}: {body}"
        );
    }
}
