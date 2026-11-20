// SPDX-License-Identifier: CC0-1.0

//! What Amazon ECS SAYS when it refuses a request for a reason it words per OPERATION,
//! driven end to end over the shared wire harness in [`crate::ecs_wire`].
//!
//! Most of this service's refusals are worded from the request alone — the member at fault
//! and what it failed — and the same words whichever operation the request named. The ones
//! below are not: Amazon ECS publishes a FIXED sentence for them, and WHICH sentence it
//! publishes is a property of the operation.
//!
//! * **A launch type outside the set an operation accepts.** The sentence lists the launch
//!   types THAT operation publishes, between brackets and comma-separated: the operations
//!   that place work on infrastructure publish `EC2,FARGATE,EXTERNAL`, which is what
//!   `CreateService$launchType` names ("The `FARGATE` launch type …", "The `EC2` launch
//!   type …", "The `EXTERNAL` launch type …") while sending a caller that wants Amazon ECS
//!   Managed Instances to `capacityProviderStrategy` with `launchType` omitted; the
//!   operations that FILTER a listing by launch type publish the wider
//!   `EC2,FARGATE,EXTERNAL,FMI,MANAGED_INSTANCES`, since a listing can be asked about
//!   capacity a placement request does not name. The table spans every operation that takes
//!   the member (`smithy_query(query="operation",operation="CreateService,RunTask,\
//!   CreateTaskSet,ListTasks,ListServices")` — the five whose input carries `launchType`)
//!   crossed with the form of the out-of-set value: a lower-case spelling of a member, a
//!   mixed-case one, a truncation, a near-miss of a member, a value that is no launch type
//!   at all, an empty string and a padded one. The two values where the published set and
//!   the MODELLED set differ are pinned beside the table: `FMI`, published by the filter
//!   operations and absent from the model, which this emulator refuses with the operation's
//!   own sentence, and `MANAGED_INSTANCES`, carried by the model and absent from the
//!   placement sentence, which a placement still serves.
//! * **A required identifier member the request leaves out.** The whole message is the
//!   sentence that operation publishes, naming no member path, no value and no count — and
//!   an operation that publishes none keeps the modelled clause the framework renders, which
//!   is the row the table closes with.
//! * **A list of identifiers that names the same one twice**, refused as the malformed
//!   request Amazon ECS calls it, on the list as the request spells it — so the refusal does
//!   not depend on what its entries resolve to, while a list of entries that each name a
//!   different service is served, one answer per entry.
//! * **A page token a listing never minted**, refused under the class and the member name
//!   the operation publishes — and, where the listing reads a token's LENGTH before reading
//!   the token, for that length.
//! * **A deployment strategy, or a LISTING's scheduling strategy, outside its set**, each
//!   worded as that operation words it rather than from the member's modelled value set.
//! * **A status filter whose entries are outside the set**, reported against the LIST the
//!   entries came in — the whole list as the value, under the name the API prints for the
//!   member, with the element's own requirement nested inside the list's.
//! * **A page size outside its documented range**, naming the bound the value crossed where
//!   the listing words it that way.
//! * **A caller-supplied list longer than one call may carry**, stated by the count the
//!   operation publishes — the ARNs for the reads whose member is a list of ARNs, the items
//!   for the reads whose member is a list of ids, under the name AWS prints for the list
//!   rather than the name the wire member carries.
//! * **A refusal AWS states with no message at all**, which carries no `message` member —
//!   not one holding an empty string.
//!
//! Nothing here changes WHICH requests are refused: each test also drives the request the
//! same rule serves, and `DescribeServices` reads the accepted create back.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    answered, cluster_with_capacity, list, ok, refusal, refused, register_definition, service,
    string, ACCOUNT, SIGNED_REGION,
};

/// The cluster every request below names.
const CLUSTER: &str = "wordedplane";

/// The bridge-mode task-definition family every placement below runs, so no row needs a
/// `networkConfiguration` and every row differs only in the value under test.
const FAMILY: &str = "wordedfamily";

/// The service the fixture creates, which the reads below report on.
const SERVICE: &str = "worded-service";

/// The one error code this service models for a request whose parameters it will not serve.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The sentence the operations that PLACE work on infrastructure publish.
const PLACEMENT_SET: &str = "launch type should be one of [EC2,FARGATE,EXTERNAL]";

/// The sentence the operations that FILTER a listing by launch type publish.
const FILTER_SET: &str = "launch type should be one of [EC2,FARGATE,EXTERNAL,FMI,MANAGED_INSTANCES]";

/// How many service deployments one `DescribeServiceDeployments` may name —
/// `DescribeServiceDeploymentsRequest$serviceDeploymentArns`: "You can specify a maximum of
/// 20 ARNs." The daemon-side reads and `DescribeServiceRevisions` document the same cap.
const MAX_ARNS: usize = 20;

/// How many tasks one `DescribeTasks` may name — `DescribeTasksRequest$tasks` is a
/// `StringList`, and Amazon ECS bounds one call at 100 of them.
const MAX_TASKS: usize = 100;

/// A router holding one cluster with capacity, one bridge-mode revision of [`FAMILY`] and
/// one service of [`SERVICE`] — so every refusal below is earned for the value under test
/// rather than for a resource the request could not resolve.
async fn fixture() -> MultiServiceRouter {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "fixture").await;
    register_definition(&router, FAMILY, "fixture").await;
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": SERVICE,
            "taskDefinition": FAMILY,
            "desiredCount": 0,
            "launchType": "EC2",
        }),
        "fixture",
    )
    .await;
    router
}

/// The request `operation` carries `launch_type` in, with every member that operation
/// requires beside it — so the request is refused for its launch type and not for a member
/// it left out.
fn launch_request(operation: &str, launch_type: &str) -> Value {
    match operation {
        "CreateService" => json!({
            "cluster": CLUSTER,
            "serviceName": "refused-launch",
            "taskDefinition": FAMILY,
            "desiredCount": 0,
            "launchType": launch_type,
        }),
        "RunTask" => json!({
            "cluster": CLUSTER,
            "taskDefinition": FAMILY,
            "launchType": launch_type,
        }),
        "CreateTaskSet" => json!({
            "cluster": CLUSTER,
            "service": SERVICE,
            "taskDefinition": FAMILY,
            "launchType": launch_type,
        }),
        "ListTasks" => json!({ "cluster": CLUSTER, "launchType": launch_type }),
        "ListServices" => json!({ "cluster": CLUSTER, "launchType": launch_type }),
        other => panic!("case {other:?}: the suite builds no request for this operation"),
    }
}

#[tokio::test]
async fn a_launch_type_outside_the_set_is_refused_with_the_set_the_operation_publishes() {
    let router = fixture().await;

    // Every operation whose input carries `launchType`, paired with the set that operation
    // publishes. `CreateTaskSet` and `RunTask` are rows the recorded divergence did not
    // name: the rule is a property of what the operation does with the member, not of the
    // operations it was found on.
    let operations: [(&str, &str); 5] = [
        ("CreateService", PLACEMENT_SET),
        ("RunTask", PLACEMENT_SET),
        ("CreateTaskSet", PLACEMENT_SET),
        ("ListTasks", FILTER_SET),
        ("ListServices", FILTER_SET),
    ];

    // The forms a value outside the set takes. None of them may reach the sentence.
    let values: [(&str, &str); 7] = [
        ("a lower-case spelling of a member", "fargate"),
        ("a mixed-case spelling of a member", "Ec2"),
        ("a truncated member", "EXTERNA"),
        ("a near-miss of a member", "MANAGED_INSTANCE"),
        ("a value that is no launch type at all", "SPOT"),
        ("an empty value", ""),
        ("a member padded with spaces", " EC2 "),
    ];

    for (operation, expected) in operations {
        for (form, value) in values {
            let case = format!("{operation} with {form}");
            let (code, message) = refusal(
                &router,
                operation,
                launch_request(operation, value),
                &case,
            )
            .await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                (INVALID_PARAMETER, expected),
                "case {case:?}: expected the launch types {operation} publishes, got {code:?} \
                 with {message:?}"
            );
            // The sentence never carries the member PATH the value sat at.
            assert!(
                !message.contains("launchType"),
                "case {case:?}: the sentence must not state the member path, got {message:?}"
            );
        }
    }

    // The two values where the published set and the set the member MODELS differ, each
    // pinned so the difference is deliberate rather than silent.
    //
    // `FMI` is published by the filter operations and is no member of the modelled
    // `LaunchType`, so this emulator cannot serve a listing filtered on it: it is refused,
    // with the sentence that operation publishes.
    for operation in ["ListTasks", "ListServices"] {
        let case = format!("{operation} filtering on the published FMI the model omits");
        let (code, message) = refusal(
            &router,
            operation,
            launch_request(operation, "FMI"),
            &case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (INVALID_PARAMETER, FILTER_SET),
            "case {case:?}: expected the operation's own sentence, got {code:?} with \
             {message:?}"
        );
    }

    // `MANAGED_INSTANCES` is the other direction: the placement sentence does not list it,
    // the model does carry it, and this service places a service with it — so a placement
    // that names it is SERVED, and only the sentence an out-of-set value earns is the
    // published three.
    for (case, operation, name) in [
        ("a create placed with MANAGED_INSTANCES", "CreateService", "managed-launch"),
        ("a task set placed with MANAGED_INSTANCES", "CreateTaskSet", "managed-launch-set"),
    ] {
        let mut request = launch_request(operation, "MANAGED_INSTANCES");
        if operation == "CreateService" {
            request["serviceName"] = json!(name);
        }
        let (status, body) = answered(&router, operation, request).await;
        assert_ne!(
            body["message"].as_str(),
            Some(PLACEMENT_SET),
            "case {case:?}: a member the model carries must not earn the out-of-set \
             sentence, got {status} with {body}"
        );
    }
}

#[tokio::test]
async fn a_launch_type_the_member_takes_is_served_and_the_created_service_reads_back() {
    let router = fixture().await;

    // A member of the set is still served by a placement, and the stored service reports the
    // launch type it was placed with.
    let case = "a create naming a member of the set";
    let created = ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": "accepted-launch",
            "taskDefinition": FAMILY,
            "desiredCount": 0,
            "launchType": "EXTERNAL",
        }),
        case,
    )
    .await;
    assert_eq!(
        string(&created["service"], "launchType", case),
        "EXTERNAL",
        "case {case:?}: expected the create to report the launch type it was given, got \
         {created}"
    );

    let described = ok(
        &router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": ["accepted-launch"] }),
        case,
    )
    .await;
    let services = list(&described, "services", case);
    assert_eq!(
        services.len(),
        1,
        "case {case:?}: expected the created service described, got {described}"
    );
    assert_eq!(
        string(&services[0], "launchType", case),
        "EXTERNAL",
        "case {case:?}: expected the stored launch type back, got {described}"
    );

    // And a filter naming a member only the LISTINGS publish is served as a filter, not
    // refused: it selects the services placed with that launch type, of which this cluster
    // holds none.
    let case = "a listing filtering on a member of its own set";
    let listed = ok(
        &router,
        "ListServices",
        json!({ "cluster": CLUSTER, "launchType": "MANAGED_INSTANCES" }),
        case,
    )
    .await;
    assert!(
        list(&listed, "serviceArns", case).is_empty(),
        "case {case:?}: expected the filter served and matching nothing, got {listed}"
    );

    // A refused create stored nothing, so the name it asked for still describes no service.
    let case = "the refused name";
    let described = ok(
        &router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": ["refused-launch"] }),
        case,
    )
    .await;
    assert!(
        list(&described, "services", case).is_empty(),
        "case {case:?}: a refused create must leave no service, got {described}"
    );
}

#[tokio::test]
async fn a_required_identifier_left_out_is_refused_with_the_sentence_its_operation_publishes() {
    let router = fixture().await;

    // The two operations that publish their own sentence for the identifier they require,
    // and one that publishes none — whose refusal is the modelled clause the framework
    // renders from the member alone, stating the value, the position and the requirement.
    let rows: [(&str, &str, Value, &str); 3] = [
        (
            "ListServiceDeployments with no service",
            "ListServiceDeployments",
            json!({ "cluster": CLUSTER }),
            "The Service ARN provided is invalid.",
        ),
        (
            "DescribeServices with no services",
            "DescribeServices",
            json!({ "cluster": CLUSTER }),
            "Services cannot be empty",
        ),
        (
            "DescribeTasks with no tasks, which publishes no sentence of its own",
            "DescribeTasks",
            json!({ "cluster": CLUSTER }),
            "1 validation error detected: Value null at 'tasks' failed to satisfy \
             constraint: Member must not be null",
        ),
    ];

    for (case, operation, request, expected) in rows {
        let (code, message) = refusal(&router, operation, request, case).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (INVALID_PARAMETER, expected),
            "case {case:?}: expected the sentence {operation} publishes for the identifier \
             it requires, got {code:?} with {message:?}"
        );
    }

    // The sentence is for a request that carries no identifier at all: one that carries an
    // identifier is resolved, and answers about what it named.
    let case = "ListServiceDeployments naming a service that is not stored";
    let (code, message) = refusal(
        &router,
        "ListServiceDeployments",
        json!({ "cluster": CLUSTER, "service": "nosuchservice" }),
        case,
    )
    .await;
    assert_eq!(
        code, "ServiceNotFoundException",
        "case {case:?}: expected the identifier resolved rather than refused for absence, \
         got {code:?} with {message:?}"
    );

    let case = "DescribeServices naming the stored service";
    let described = ok(
        &router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [SERVICE] }),
        case,
    )
    .await;
    assert_eq!(
        list(&described, "services", case).len(),
        1,
        "case {case:?}: expected the stored service described, got {described}"
    );
}

#[tokio::test]
async fn a_page_token_the_listing_never_minted_is_refused_naming_the_member_it_came_in() {
    let router = fixture().await;

    // The forms a token this listing cannot resume from takes: a value no listing minted, a
    // value another listing's scope minted, and one that is a valid ARN of this service but
    // no cursor.
    let listed = ok(
        &router,
        "ListServices",
        json!({ "cluster": CLUSTER }),
        "setup",
    )
    .await;
    let arns = list(&listed, "serviceArns", "setup");
    let other = arns
        .first()
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case \"setup\": expected the stored service listed, got {listed}"))
        .to_string();
    let tokens: [(&str, String); 3] = [
        ("a token that is not a cursor at all", "not-a-token".to_string()),
        ("a token of this service's own ARN space", other),
        ("an empty token", String::new()),
    ];

    for (case, token) in tokens {
        let body = refused(
            &router,
            "ListServiceDeployments",
            json!({ "cluster": CLUSTER, "service": SERVICE, "nextToken": token }),
            INVALID_PARAMETER,
            case,
        )
        .await;
        assert_eq!(
            body["message"].as_str(),
            Some("Invalid parameter: NextToken"),
            "case {case:?}: expected the member the token came in to be named, got {body}"
        );
    }

    // A listing that sends back the token it was given resumes from it, so the refusal is
    // about a cursor this listing never minted.
    let case = "the token this listing minted";
    let first = ok(
        &router,
        "ListServiceDeployments",
        json!({ "cluster": CLUSTER, "service": SERVICE, "maxResults": 1 }),
        case,
    )
    .await;
    assert_eq!(
        list(&first, "serviceDeployments", case).len(),
        1,
        "case {case:?}: expected one deployment on the first page, got {first}"
    );
}

#[tokio::test]
async fn a_deployment_strategy_outside_its_set_is_refused_with_one_fixed_sentence() {
    let router = fixture().await;

    // The sentence names neither the strategies the member takes nor the value the request
    // carried, so the rows differ only in the FORM of the out-of-set value and in which
    // operation carries the configuration.
    let values: [(&str, &str); 5] = [
        ("a lower-case spelling of a member", "rolling"),
        ("a mixed-case spelling of a member", "Blue_Green"),
        ("a hyphenated spelling of a member", "BLUE-GREEN"),
        ("a strategy that is no deployment strategy", "RAINBOW"),
        ("an empty value", ""),
    ];

    for (form, value) in values {
        let rows: [(&str, Value); 2] = [
            (
                "CreateService",
                json!({
                    "cluster": CLUSTER,
                    "serviceName": "refused-strategy",
                    "taskDefinition": FAMILY,
                    "desiredCount": 0,
                    "deploymentConfiguration": { "strategy": value },
                }),
            ),
            (
                "UpdateService",
                json!({
                    "cluster": CLUSTER,
                    "service": SERVICE,
                    "deploymentConfiguration": { "strategy": value },
                }),
            ),
        ];
        for (operation, request) in rows {
            let case = format!("{operation} with {form}");
            let (code, message) = refusal(&router, operation, request, &case).await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                (INVALID_PARAMETER, "Invalid deployment configuration strategy."),
                "case {case:?}: expected the one sentence the member's refusal carries, got \
                 {code:?} with {message:?}"
            );
        }
    }

    // A member of the set is still served, and the stored configuration reads back.
    let case = "a create naming a member of the set";
    let created = ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": "accepted-strategy",
            "taskDefinition": FAMILY,
            "desiredCount": 0,
            "deploymentConfiguration": { "strategy": "ROLLING" },
        }),
        case,
    )
    .await;
    assert_eq!(
        created["service"]["deploymentConfiguration"]["strategy"].as_str(),
        Some("ROLLING"),
        "case {case:?}: expected the strategy the create was given, got {created}"
    );
}

#[tokio::test]
async fn a_listings_scheduling_strategy_outside_its_set_is_refused_as_the_listing_words_it() {
    let router = fixture().await;

    // The listing opens the sentence on the member and lists the two strategies in the order
    // it publishes them — not in the words a CREATE words its own refusal in, which
    // `crate::enum_value_refusals` pins.
    for (form, value) in [
        ("a lower-case spelling of a member", "replica"),
        ("a mixed-case spelling of a member", "Daemon"),
        ("a value that is no scheduling strategy", "SINGLETON"),
        ("an empty value", ""),
    ] {
        let case = format!("ListServices with {form}");
        let (code, message) = refusal(
            &router,
            "ListServices",
            json!({ "cluster": CLUSTER, "schedulingStrategy": value }),
            &case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (
                INVALID_PARAMETER,
                "Scheduling strategy should be one of [REPLICA,DAEMON]"
            ),
            "case {case:?}: expected the sentence the listing words, got {code:?} with \
             {message:?}"
        );
    }

    // Each member of the set is served as a filter.
    for strategy in ["REPLICA", "DAEMON"] {
        let case = format!("ListServices filtering on {strategy}");
        let listed = ok(
            &router,
            "ListServices",
            json!({ "cluster": CLUSTER, "schedulingStrategy": strategy }),
            &case,
        )
        .await;
        let reported = list(&listed, "serviceArns", &case).len();
        assert_eq!(
            reported,
            usize::from(strategy == "REPLICA"),
            "case {case:?}: expected the filter to select the services placed with it, got \
             {listed}"
        );
    }
}

#[tokio::test]
async fn a_status_filter_entry_outside_the_set_is_reported_against_the_list_it_came_in() {
    let router = fixture().await;

    // The clause names the LIST — the whole list as the value, under the name the API prints
    // for it — and nests the element's own requirement inside the list's, with the value set
    // in the ascending order this service states its value sets in. The rows differ in how
    // many entries are outside the set and where they sit, because one list is ONE violation
    // however many of its entries are at fault.
    let accepted = "IN_PROGRESS, PENDING, ROLLBACK_FAILED, ROLLBACK_IN_PROGRESS, \
                    ROLLBACK_REQUESTED, ROLLBACK_SUCCESSFUL, STOPPED, STOP_REQUESTED, \
                    SUCCESSFUL";
    let rows: [(&str, Vec<&str>); 4] = [
        ("one entry in the wrong case", vec!["successful"]),
        ("one entry that names no status", vec!["ROLLED_BACK"]),
        (
            "a bad entry after a good one",
            vec!["SUCCESSFUL", "successful"],
        ),
        ("two bad entries", vec!["pending", "stopped"]),
    ];

    for (case, statuses) in rows {
        let (code, message) = refusal(
            &router,
            "ListServiceDeployments",
            json!({ "cluster": CLUSTER, "service": SERVICE, "status": statuses }),
            case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (
                INVALID_PARAMETER,
                format!(
                    "1 validation error detected: Value '[{}]' at 'statusFilter' failed to \
                     satisfy constraint: Member must satisfy constraint: [Member must \
                     satisfy enum value set: [{accepted}]]",
                    statuses.join(", ")
                )
                .as_str()
            ),
            "case {case:?}: expected the clause the list earns, got {code:?} with {message:?}"
        );
    }

    // A filter whose every entry names a status is served as a filter: the status the
    // fixture's deployment reports selects it, and another status selects nothing.
    let case = "the whole history";
    let listed = ok(
        &router,
        "ListServiceDeployments",
        json!({ "cluster": CLUSTER, "service": SERVICE }),
        case,
    )
    .await;
    let deployments = list(&listed, "serviceDeployments", case);
    assert_eq!(
        deployments.len(),
        1,
        "case {case:?}: expected the create's own deployment, got {listed}"
    );
    let carried = string(&deployments[0], "status", case);
    let other = "ROLLBACK_FAILED";
    assert_ne!(
        carried, other,
        "case {case:?}: the row that must select nothing has to name another status"
    );
    for (case, status, expected) in [
        ("the status the deployment carries", carried.as_str(), 1),
        ("a status it does not carry", other, 0),
    ] {
        let listed = ok(
            &router,
            "ListServiceDeployments",
            json!({ "cluster": CLUSTER, "service": SERVICE, "status": [status] }),
            case,
        )
        .await;
        assert_eq!(
            list(&listed, "serviceDeployments", case).len(),
            expected,
            "case {case:?}: expected the filter served, got {listed}"
        );
    }
}

#[tokio::test]
async fn a_listings_page_size_outside_its_range_names_the_bound_it_crossed() {
    let router = fixture().await;

    // `ListServices$maxResults`: "This value can be between 1 and 100." The listing names
    // the bound the value crossed at EITHER end, so the rows span both ends, one step and
    // far past each, and the floor of the member's own type.
    let rows: [(&str, i64, &str); 6] = [
        ("one below the range", 0, "Max results can not be lesser than 1"),
        ("far below the range", -37, "Max results can not be lesser than 1"),
        (
            "at the floor of the member's type",
            i64::from(i32::MIN),
            "Max results can not be lesser than 1",
        ),
        (
            "one above the range",
            101,
            "Max results can not be greater than 100",
        ),
        (
            "far above the range",
            5_000,
            "Max results can not be greater than 100",
        ),
        (
            "at the ceiling of the member's type",
            i64::from(i32::MAX),
            "Max results can not be greater than 100",
        ),
    ];

    for (case, max_results, expected) in rows {
        let (code, message) = refusal(
            &router,
            "ListServices",
            json!({ "cluster": CLUSTER, "maxResults": max_results }),
            case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (INVALID_PARAMETER, expected),
            "case {case:?}: expected the bound the page size crossed, got {code:?} with \
             {message:?}"
        );
    }

    // Each published bound is INSIDE the range, so each is served.
    for (case, max_results) in [("the smallest page", 1), ("the largest page", 100)] {
        let listed = ok(
            &router,
            "ListServices",
            json!({ "cluster": CLUSTER, "maxResults": max_results }),
            case,
        )
        .await;
        assert_eq!(
            list(&listed, "serviceArns", case).len(),
            1,
            "case {case:?}: expected the cluster's one service on the page, got {listed}"
        );
    }
}

#[tokio::test]
async fn a_token_shorter_than_the_shortest_one_the_service_mints_is_refused_for_its_length() {
    let router = fixture().await;

    // `ListServices` reads a token's LENGTH before reading the token: a value shorter than
    // the shortest continuation token Amazon ECS hands out is refused for that length. The
    // rows span the forms a short value takes, including one exactly one character short.
    let short_sentence = "token shorter than 48.";
    let rows: [(&str, String, &str); 5] = [
        ("an empty token", String::new(), short_sentence),
        ("a word", "not-a-token".to_string(), short_sentence),
        (
            "a value one character short",
            "a".repeat(47),
            short_sentence,
        ),
        (
            "a value of exactly the shortest length, naming no page",
            "a".repeat(48),
            "Invalid nextToken.",
        ),
        (
            "a long value naming no page",
            "f".repeat(96),
            "Invalid nextToken.",
        ),
    ];

    for (case, token, expected) in rows {
        let (code, message) = refusal(
            &router,
            "ListServices",
            json!({ "cluster": CLUSTER, "nextToken": token }),
            case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (INVALID_PARAMETER, expected),
            "case {case:?}: expected the sentence this listing answers for the value's \
             length, got {code:?} with {message:?}"
        );
    }

    // The length rule never refuses a token the listing MINTED: a page walked one entry at a
    // time resumes through the token it was handed.
    let case = "the token this listing minted";
    let first = ok(
        &router,
        "ListServices",
        json!({ "cluster": CLUSTER, "maxResults": 1 }),
        case,
    )
    .await;
    assert_eq!(
        list(&first, "serviceArns", case).len(),
        1,
        "case {case:?}: expected one service on the first page, got {first}"
    );
    if let Some(token) = first["nextToken"].as_str() {
        let resumed = ok(
            &router,
            "ListServices",
            json!({ "cluster": CLUSTER, "maxResults": 1, "nextToken": token }),
            case,
        )
        .await;
        assert!(
            resumed["serviceArns"].is_array(),
            "case {case:?}: expected the minted token to resume the walk, got {resumed}"
        );
    }
}

/// One bounded list of ARNs: the operation, the member it carries them in, and the resource
/// the sentence names.
type ArnList = (&'static str, &'static str, &'static str);

#[tokio::test]
async fn an_over_long_list_of_arns_is_refused_by_the_count_of_arns_the_call_may_carry() {
    let router = fixture().await;

    // Every read whose required member is a LIST OF ARNS with a documented per-call cap.
    // The daemon-side pair is not a read the recorded divergence named: the sentence belongs
    // to the member's kind, so it is the same for every read that takes one.
    let lists: [ArnList; 4] = [
        (
            "DescribeServiceDeployments",
            "serviceDeploymentArns",
            "Service Deployment",
        ),
        (
            "DescribeServiceRevisions",
            "serviceRevisionArns",
            "Service Revision",
        ),
        (
            "DescribeDaemonDeployments",
            "daemonDeploymentArns",
            "Daemon Deployment",
        ),
        (
            "DescribeDaemonRevisions",
            "daemonRevisionArns",
            "Daemon Revision",
        ),
    ];

    for (operation, member, resource) in lists {
        let expected = format!("{resource} ARN count cannot exceed {MAX_ARNS}");
        // One past the cap is refused, with the count the call may carry.
        let case = format!("{operation} naming one ARN too many");
        let arns: Vec<String> = (0..=MAX_ARNS).map(|index| arn(resource, index)).collect();
        let (code, message) =
            refusal(&router, operation, json!({ member: arns }), &case).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (INVALID_PARAMETER, expected.as_str()),
            "case {case:?}: expected the ARN count to be named, got {code:?} with {message:?}"
        );

        // AT the cap is not: whatever the call then answers about ARNs that name nothing, it
        // is not this refusal.
        let case = format!("{operation} naming exactly as many ARNs as it may");
        let arns: Vec<String> = (0..MAX_ARNS).map(|index| arn(resource, index)).collect();
        let (_, body) = answered(&router, operation, json!({ member: arns })).await;
        assert_ne!(
            body["message"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: the cap must bound more than it may carry, not what it may, got \
             {body}"
        );
    }
}

/// One ARN of `resource` under the fixture's cluster and service, as this service writes an
/// ARN of that resource: the resource type is its name lower-cased with its words hyphenated.
fn arn(resource: &str, index: usize) -> String {
    let resource_type = resource.to_lowercase().replace(' ', "-");
    format!(
        "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:{resource_type}/{CLUSTER}/{SERVICE}/{index:019}"
    )
}

#[tokio::test]
async fn an_over_long_list_of_task_ids_is_refused_under_the_name_the_service_publishes() {
    let router = fixture().await;

    // The member is `tasks`; the name Amazon ECS prints for the list it holds is `taskIds`.
    let expected = format!("taskIds can have at most {MAX_TASKS} items.");

    let case = "DescribeTasks naming one task too many";
    let ids: Vec<String> = (0..=MAX_TASKS).map(|index| format!("{index:032x}")).collect();
    let (code, message) = refusal(
        &router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": ids }),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (INVALID_PARAMETER, expected.as_str()),
        "case {case:?}: expected the bound stated under the published name, got {code:?} \
         with {message:?}"
    );
    assert!(
        !message.contains("tasks can have"),
        "case {case:?}: the sentence must not substitute the wire member's name, got \
         {message:?}"
    );

    // At the cap the read is served, and every id that names no task of the cluster is one
    // MISSING failure.
    let case = "DescribeTasks naming exactly as many tasks as it may";
    let ids: Vec<String> = (0..MAX_TASKS).map(|index| format!("{index:032x}")).collect();
    let described = ok(
        &router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": ids }),
        case,
    )
    .await;
    assert_eq!(
        list(&described, "failures", case).len(),
        MAX_TASKS,
        "case {case:?}: expected one failure per id the cluster holds no task for, got \
         {described}"
    );
}

#[tokio::test]
async fn a_delete_of_a_service_that_resolves_to_nothing_carries_no_message_member() {
    let router = fixture().await;

    // The names differ in shape, including one the stored service's own name extends: the
    // refusal is the operation's, not the value's.
    for name in ["nosuchservice", "wo", "worded-service2", "a_long_name_42"] {
        let case = format!("DeleteService naming {name:?}");
        let body = refused(
            &router,
            "DeleteService",
            json!({ "cluster": CLUSTER, "service": name }),
            "ServiceNotFoundException",
            &case,
        )
        .await;
        assert!(
            body.get("message").is_none(),
            "case {case:?}: expected no message member at all, got {body}"
        );
    }

    // The service that IS stored is deleted by the same call, so the refusals above are
    // about the name resolving to nothing.
    let case = "the stored service";
    let deleted = ok(
        &router,
        "DeleteService",
        json!({ "cluster": CLUSTER, "service": SERVICE }),
        case,
    )
    .await;
    assert_eq!(
        string(&deleted["service"], "serviceName", case),
        SERVICE,
        "case {case:?}: expected the stored service deleted, got {deleted}"
    );
}

/// The sentence a `services` list that names one service more than once earns: fixed text,
/// naming neither the repeated entry nor how many times it was repeated.
const DUPLICATE_SERVICES: &str = "Malformed request with duplicate service names.";

#[tokio::test]
async fn a_describe_naming_one_service_twice_is_refused_as_a_malformed_request() {
    let router = fixture().await;

    // The second service lets a row repeat an entry with ANOTHER name beside it, so a
    // refusal cannot come from the list's length or from its one name.
    let second = "worded-second";
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": second,
            "taskDefinition": FAMILY,
            "desiredCount": 0,
            "launchType": "EC2",
        }),
        "fixture",
    )
    .await;

    // The rows differ along WHERE the repeat sits and WHAT it repeats: the whole list, a
    // repeat after another name, one before it, a name repeated three times, and a name that
    // names no stored service at all — the list is judged as the request spells it, before
    // any entry is resolved, so a repeat of a name that would have come back as a MISSING
    // failure is refused just the same.
    let rows: [(&str, Vec<&str>); 6] = [
        ("the same name twice", vec![SERVICE, SERVICE]),
        ("a repeat after another name", vec![second, SERVICE, SERVICE]),
        ("a repeat before another name", vec![SERVICE, SERVICE, second]),
        ("a name repeated three times", vec![SERVICE, SERVICE, SERVICE]),
        (
            "a name no service is stored under, repeated",
            vec!["nosuchservice", "nosuchservice"],
        ),
        (
            "two names each repeated once",
            vec![SERVICE, second, SERVICE, second],
        ),
    ];
    for (case, services) in rows {
        let (code, message) = refusal(
            &router,
            "DescribeServices",
            json!({ "cluster": CLUSTER, "services": services }),
            case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (INVALID_PARAMETER, DUPLICATE_SERVICES),
            "case {case:?}: expected the malformed-request sentence, got {code:?} with \
             {message:?}"
        );
        // The sentence echoes nothing of the list the request carried.
        assert!(
            !message.contains(SERVICE),
            "case {case:?}: the sentence must not echo an entry of the list, got {message:?}"
        );
    }

    // A list whose entries each name a different service is SERVED, and reports one answer
    // per entry: a stored service under `services`, a name nothing is stored under as a
    // `MISSING` failure.
    let rows: [(&str, Vec<&str>, usize, usize); 4] = [
        ("one stored service", vec![SERVICE], 1, 0),
        ("two different stored services", vec![SERVICE, second], 2, 0),
        (
            "a stored service beside a name nothing is stored under",
            vec![SERVICE, "nosuchservice"],
            1,
            1,
        ),
        (
            "two different names nothing is stored under",
            vec!["nosuchservice", "anothermissing"],
            0,
            2,
        ),
    ];
    for (case, services, described, missing) in rows {
        let body = ok(
            &router,
            "DescribeServices",
            json!({ "cluster": CLUSTER, "services": services }),
            case,
        )
        .await;
        assert_eq!(
            (
                list(&body, "services", case).len(),
                list(&body, "failures", case).len()
            ),
            (described, missing),
            "case {case:?}: expected {described} described and {missing} failed, got {body}"
        );
    }
}
