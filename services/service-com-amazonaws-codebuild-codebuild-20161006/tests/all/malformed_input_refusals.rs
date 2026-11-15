// SPDX-License-Identifier: CC0-1.0

//! Cover for how CodeBuild refuses a request it cannot read or cannot act on, driven over
//! the wire the router serves: `POST /` with `X-Amz-Target: CodeBuild_20161006.<Operation>`
//! on awsJson1_1.
//!
//! Four rules are under test, each about a refusal whose only distinguishing payload is its
//! error class and its words:
//!
//! * a value whose JSON class the modelled type of the member it is bound to cannot be read
//!   from fails DESERIALIZATION, before any rule the model declares is checked: `400`
//!   `SerializationException` with the reason under the capital-`Message` member, stating the
//!   JSON form that member is read from;
//! * an input list of resource identifiers a request leaves empty or omits names nothing to
//!   act on: `400 InvalidInputException` stating the KIND of list, with nothing of the
//!   request interpolated;
//! * a project NAME carried as the empty string is no project identifier: `400
//!   InvalidInputException` stating that a project name cannot be empty;
//! * a sort criterion outside its enumerated type: `400 InvalidInputException` naming the
//!   member and quoting the value exactly as the request wrote it.
//!
//! Each table spans the class the rule is about — several operations, every modelled kind a
//! member can be, both forms of "no list at all", and several shapes of unmodelled value —
//! rather than one recorded request, and each row's control shows what the rule does NOT
//! refuse.

use serde_json::{json, Value};
use service_com_amazonaws_codebuild_codebuild_20161006::ops::types::PROJECT_SORT_BY_TYPE_VALUES;
use service_com_amazonaws_codebuild_codebuild_20161006::state;
use smithy_server_runtime::MultiServiceRouter;

use super::project_build_lifecycle::{call, create_project, emulator, error_type, REGION};

/// The `message` an error body carries — the member every error shape this service's own
/// model declares states its sentence in.
fn message(body: &Value) -> String {
    body["message"].as_str().unwrap_or_default().to_string()
}

/// The `Message` an error body carries — the member a protocol-level serialization refusal
/// states its reason in.
fn capital_message(body: &Value) -> String {
    body["Message"].as_str().unwrap_or_default().to_string()
}

/// Call `operation`, assert it was refused with HTTP 400 `InvalidInputException`, and hand
/// back the sentence it stated.
async fn invalid_input(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> String {
    let (status, answer) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400 from {operation}, got {status} with {answer}"
    );
    assert_eq!(
        error_type(&answer),
        "InvalidInputException",
        "case {case:?}: expected InvalidInputException from {operation}, got {answer}"
    );
    message(&answer)
}

/// A request for `operation` carrying `value` at `member`, plus whatever else that
/// operation requires for the refusal under test to be the one it answers.
fn request(operation: &str, member: &str, value: Value) -> Value {
    let mut body = match operation {
        // The report the coverage read is about: `@required`, so a request that omits it is
        // refused for its absence instead of for the member under test.
        "DescribeCodeCoverages" => {
            json!({"reportArn": state::report_group_arn(REGION, "cover-group:0")})
        }
        _ => json!({}),
    };
    if let Value::Object(members) = &mut body {
        members.insert(member.to_string(), value);
    }
    body
}

/// RULE: a value whose JSON class the modelled type of the member it is bound to cannot be
/// read from never becomes a value at all, so it is refused at the deserialization layer
/// with `400 SerializationException`, whose capital-`Message` member states the JSON form
/// that member IS read from (`Expected <form> or null`) and quotes nothing of the request.
///
/// The rows span every modelled kind a top-level member of this service's inputs can be —
/// `smithy_query(query="operation",
/// operation="BatchGetProjects,BatchGetBuilds,CreateProject,StartBuild,ListProjects,ListReportGroups")`
/// gives each member's target — crossed with the JSON classes that cannot be read into it.
#[tokio::test]
async fn a_value_the_modelled_member_cannot_be_read_from_is_a_serialization_refusal() {
    let router = emulator();

    // (operation, member, the value sent, the JSON form that member is read from)
    let rows: [(&str, &str, Value, &str); 12] = [
        ("BatchGetProjects", "names", json!("not-a-list"), "list"),
        ("BatchGetProjects", "names", json!({"0": "a"}), "list"),
        ("BatchGetProjects", "names", json!(7), "list"),
        ("BatchGetBuilds", "ids", json!("not-a-list"), "list"),
        ("BatchDeleteBuilds", "ids", json!(true), "list"),
        ("CreateProject", "name", json!(["a-list"]), "string"),
        ("CreateProject", "source", json!("NO_SOURCE"), "structure"),
        ("CreateProject", "tags", json!({"k": "v"}), "list"),
        ("CreateProject", "badgeEnabled", json!("true"), "boolean"),
        ("CreateProject", "timeoutInMinutes", json!("60"), "number"),
        ("ListProjects", "sortBy", json!(["NAME"]), "string"),
        ("ListReportGroups", "maxResults", json!("10"), "number"),
    ];

    for (operation, member, value, form) in rows {
        let case = format!("{operation} with {value} at {member}");
        let (status, answer) = call(&router, operation, request(operation, member, value.clone()))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {answer}"
        );
        assert_eq!(
            error_type(&answer),
            "SerializationException",
            "case {case:?}: a body that cannot be read is a serialization refusal, got {answer}"
        );
        let stated = capital_message(&answer);
        assert_eq!(
            stated,
            format!("Expected {form} or null"),
            "case {case:?}: expected the form the member is read from, got {stated:?}"
        );
        assert_eq!(
            message(&answer),
            "",
            "case {case:?}: a serialization refusal states its reason only under Message, \
             got {answer}"
        );
        assert!(
            !stated.contains(&value.to_string()),
            "case {case:?}: the sentence must not quote the value back, got {stated:?}"
        );
    }
}

/// RULE: the deserialization layer refuses only what cannot be READ — a value of the
/// member's own class, and JSON `null`, reach the layer that checks the rules the model
/// declares, where this service answers `InvalidInputException` instead.
#[tokio::test]
async fn a_readable_value_reaches_the_rules_the_model_declares() {
    let router = emulator();

    // A list IS readable into the list member, so an empty one is the model's rule, not the
    // codec's; and `null` is absence, which is the same rule again.
    for (case, value) in [("an empty list", json!([])), ("null", json!(null))] {
        let stated = invalid_input(
            &router,
            case,
            "BatchGetProjects",
            json!({"names": value.clone()}),
        )
        .await;
        assert_eq!(
            stated, "The input project name list cannot be empty.",
            "case {case:?}: expected the empty-list sentence, got {stated:?}"
        );
    }

    // A readable body is served: the same member, carrying a list of names.
    let (status, answer) = call(
        &router,
        "BatchGetProjects",
        json!({"names": ["no-such-project"]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "a readable request must be served, got {status} with {answer}"
    );
    assert_eq!(
        answer["projectsNotFound"],
        json!(["no-such-project"]),
        "the name nothing is held under belongs in the absent half, got {answer}"
    );
}

/// RULE: an input list of resource identifiers a request leaves EMPTY or omits names
/// nothing to act on, and is refused with 400 `InvalidInputException` worded `The input
/// <kind> list cannot be empty.` — fixed text naming the KIND of identifier the list holds,
/// with neither the member path, the size nor the operation interpolated, so the sentence is
/// the same for every operation whose input carries that list.
///
/// The rows span the three operations whose input is such a list and all three forms of "no
/// list at all": the member omitted, sent as `null`, and sent as a list with nothing in it.
/// A list whose sentence is NOT recorded keeps the shared constraint envelope, which the
/// last rows pin.
#[tokio::test]
async fn an_identifier_list_naming_nothing_is_refused_in_codebuilds_own_words() {
    let router = emulator();

    // (operation, the member that carries the list, the sentence CodeBuild states)
    let lists: [(&str, &str, &str); 3] = [
        (
            "BatchGetProjects",
            "names",
            "The input project name list cannot be empty.",
        ),
        (
            "BatchGetBuilds",
            "ids",
            "The input build ID list cannot be empty.",
        ),
        (
            "BatchDeleteBuilds",
            "ids",
            "The input build ID list cannot be empty.",
        ),
    ];

    for (operation, member, expected) in lists {
        // (how the request names nothing, the body it sends)
        let forms: [(&str, Value); 3] = [
            ("the member omitted", json!({})),
            ("the member null", request(operation, member, json!(null))),
            ("an empty list", request(operation, member, json!([]))),
        ];
        for (form, body) in forms {
            let case = format!("{operation} with {form}");
            let stated = invalid_input(&router, &case, operation, body).await;
            assert_eq!(
                stated, expected,
                "case {case:?}: expected CodeBuild's empty-list sentence, got {stated:?}"
            );
            for leaked in [member, "items", "constraint", operation] {
                assert!(
                    !stated.contains(leaked),
                    "case {case:?}: the sentence must not name {leaked:?}, got {stated:?}"
                );
            }
        }
    }

    // The same member NAME on another operation carries another kind of identifier
    // (`BatchGetFleetsInput$names` targets `FleetNames`, not `ProjectNames`), and
    // CodeBuild's sentence for that list is not recorded — so it keeps the shared
    // constraint envelope naming the member and the constraint it failed.
    for (form, body, expected) in [
        (
            "an empty list",
            json!({"names": []}),
            "1 validation error detected: Value '0 items' at 'names' failed to satisfy \
             constraint: Member must have length greater than or equal to 1",
        ),
        (
            "the member omitted",
            json!({}),
            "1 validation error detected: Value null at 'names' failed to satisfy \
             constraint: Member must not be null",
        ),
    ] {
        let case = format!("BatchGetFleets with {form}");
        let stated = invalid_input(&router, &case, "BatchGetFleets", body).await;
        assert_eq!(
            stated, expected,
            "case {case:?}: expected the shared constraint envelope, got {stated:?}"
        );
    }
}

/// RULE: a project NAME carried as the empty string is no project identifier, and is refused
/// with 400 `InvalidInputException` worded `Project name cannot be empty` — fixed text
/// naming the kind of value that was empty, with neither the member nor the operation
/// interpolated.
///
/// The rows span both places an input carries a project name as a plain string: inside the
/// list of project names the batch read takes, wherever in that list the empty entry sits,
/// and the single-project identifier the per-project listings take.
#[tokio::test]
async fn an_empty_project_name_is_refused_in_codebuilds_own_words() {
    let router = emulator();
    create_project(&router, "empty-name-proj", json!({})).await;

    let expected = "Project name cannot be empty";

    // An empty entry alone, first beside a name this account holds, and last.
    for (case, names) in [
        ("the only entry", json!([""])),
        ("the first of two entries", json!(["", "empty-name-proj"])),
        ("the last of two entries", json!(["empty-name-proj", ""])),
    ] {
        let case = format!("BatchGetProjects with an empty name as {case}");
        let stated = invalid_input(&router, &case, "BatchGetProjects", json!({"names": names}))
            .await;
        assert_eq!(
            stated, expected,
            "case {case:?}: expected CodeBuild's empty-name sentence, got {stated:?}"
        );
    }

    // Every operation that addresses ONE project by a plain-string identifier.
    for operation in [
        "ListBuildsForProject",
        "ListBuildBatchesForProject",
        "ListSandboxesForProject",
        "StartBuild",
        "InvalidateProjectCache",
    ] {
        let case = format!("{operation} with an empty projectName");
        let stated = invalid_input(&router, &case, operation, json!({"projectName": ""})).await;
        assert_eq!(
            stated, expected,
            "case {case:?}: expected CodeBuild's empty-name sentence, got {stated:?}"
        );
    }

    // The control: the project this account holds still reads back, so nothing above
    // changed what a later caller sees.
    let (status, read) = call(
        &router,
        "BatchGetProjects",
        json!({"names": ["empty-name-proj"]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "BatchGetProjects after the refusals: expected 200, got {status} with {read}"
    );
    assert_eq!(
        read["projects"][0]["name"],
        json!("empty-name-proj"),
        "a refused request must leave the stored project as it was, got {read}"
    );
}

/// RULE: a sort criterion outside the enumerated type its member targets is refused with 400
/// `InvalidInputException` worded `Invalid sortBy type specified: <value>` — the prefix is
/// fixed and names the member, and the trailing token is the value taken from the request
/// exactly as sent, so one sentence serves every operation that takes a criterion whatever
/// its own value set is.
///
/// The rows span every operation of this service whose input models a `sortBy`
/// (`smithy_query(query="operation",
/// operation="ListProjects,ListReportGroups,ListSharedProjects,ListSharedReportGroups,ListFleets,DescribeCodeCoverages")`),
/// each targeting a value set of its own, crossed with the forms an unmodelled value takes:
/// a modelled value in the wrong case, a token of the right shape, the empty string and one
/// with a trailing space. A value the operation's own type DOES declare is served.
#[tokio::test]
async fn an_unmodelled_sort_criterion_is_refused_in_codebuilds_own_words() {
    let router = emulator();

    let operations = [
        "ListProjects",
        "ListReportGroups",
        "ListSharedProjects",
        "ListSharedReportGroups",
        "ListFleets",
        "DescribeCodeCoverages",
    ];

    for operation in operations {
        for value in ["name", "BOGUS", "", "NAME ", "created_time"] {
            let case = format!("{operation} with sortBy {value:?}");
            let stated = invalid_input(
                &router,
                &case,
                operation,
                request(operation, "sortBy", json!(value)),
            )
            .await;
            assert_eq!(
                stated,
                format!("Invalid sortBy type specified: {value}"),
                "case {case:?}: expected CodeBuild's sort-criterion sentence, got {stated:?}"
            );
            assert!(
                !stated.contains("constraint"),
                "case {case:?}: the sentence must not state a constraint, got {stated:?}"
            );
        }
    }

    // The control: every value `ProjectSortByType` declares is served — the refusal is about
    // being outside the set, not about the member being present.
    for value in PROJECT_SORT_BY_TYPE_VALUES {
        let (status, answer) = call(&router, "ListProjects", json!({"sortBy": value})).await;
        assert_eq!(
            status, 200,
            "case {value:?}: a modelled criterion must be served, got {status} with {answer}"
        );
    }

    // The sibling member that names the sort DIRECTION keeps its own fixed sentence: the
    // rule above is about the criterion, not about every enumerated member.
    let stated = invalid_input(
        &router,
        "ListProjects with sortOrder \"ascending\"",
        "ListProjects",
        json!({"sortOrder": "ascending"}),
    )
    .await;
    assert_eq!(
        stated, "Invalid sort order specified",
        "a sort direction keeps CodeBuild's own sentence for it, got {stated:?}"
    );

    // And a member that is neither keeps the shared constraint envelope.
    let project = create_project(&router, "sort-by-proj", json!({})).await;
    let stated = invalid_input(
        &router,
        "UpdateProjectVisibility with projectVisibility \"private\"",
        "UpdateProjectVisibility",
        json!({
            "projectArn": project["arn"].as_str().unwrap_or_default(),
            "projectVisibility": "private",
        }),
    )
    .await;
    assert!(
        stated.contains("failed to satisfy constraint"),
        "an enumerated member that is no sort criterion keeps the envelope, got {stated:?}"
    );

    // The create above is a control of its own: a project really was stored.
    let (status, read) = call(
        &router,
        "BatchGetProjects",
        json!({"names": ["sort-by-proj"]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "BatchGetProjects for the stored project: expected 200, got {status} with {read}"
    );
    assert_eq!(
        read["projects"][0]["name"],
        json!("sort-by-proj"),
        "the stored project reads back, got {read}"
    );
}
