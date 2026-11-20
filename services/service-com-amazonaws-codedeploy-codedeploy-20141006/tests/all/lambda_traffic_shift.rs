// SPDX-License-Identifier: CC0-1.0

//! What an Lambda CodeDeploy deployment DOES: the traffic shift its revision's
//! AppSpec describes, read where a client reads it — on the Lambda alias.
//!
//! The suite mounts AWS Lambda beside CodeDeploy on one engine and drives both
//! through the same front door a client uses: the function, its versions and its
//! alias are created through Lambda's own `restJson1` API, the deployment through
//! CodeDeploy's awsJson envelope, and the outcome is read back with `GetAlias`.
//! Nothing reaches into either store.

use serde_json::{json, Value};

use crate::codedeploy_lifecycle::{terminal_deployment, Emulator};

/// The service role every deployment group here is created with; CodeDeploy only
/// stores it.
const SERVICE_ROLE: &str = "arn:aws:iam::123456789012:role/CodeDeployRole";

/// The collection a function's aliases bind, which is also where a function is
/// created: `POST /2015-03-31/functions`.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Create a function and publish `versions` versions of it, so the versions an
/// AppSpec may target exist.
///
/// Each publish carries its own `Description`: "Lambda doesn't publish a version
/// if the function's configuration and code haven't changed since the last
/// version", so a distinct description is what makes each publish a distinct
/// snapshot with its own number.
///
/// Answers the `FunctionArn` AWS Lambda minted for it, which is what the ARN in a
/// Lambda refusal about one of its aliases is built from.
async fn given_function(emulator: &Emulator, name: &str, versions: usize) -> String {
    let (status, body) = emulator
        .lambda(
            "POST",
            FUNCTIONS_URI,
            json!({
                "FunctionName": name,
                "Role": format!("arn:aws:iam::{}:role/handler", engine::emulator_account_id()),
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Code": {"ZipFile": ""},
            }),
        )
        .await;
    assert_eq!(
        status, 201,
        "case {name:?}: CreateFunction must succeed, got {status} {body:?}"
    );
    for version in 1..=versions {
        let (status, body) = emulator
            .lambda(
                "POST",
                &format!("{FUNCTIONS_URI}/{name}/versions"),
                json!({"Description": format!("snapshot {version}")}),
            )
            .await;
        assert_eq!(
            (status, body["Version"].as_str()),
            (201, Some(version.to_string().as_str())),
            "case {name:?}: PublishVersion must publish version {version}, got {status} {body:?}"
        );
    }
    body["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: CreateFunction answered no FunctionArn"))
        .to_string()
}

/// Create the alias the deployment moves, invoking `version` and splitting its
/// invocations with `weights` when the case asks for a weighted one.
async fn given_alias(
    emulator: &Emulator,
    function: &str,
    alias: &str,
    version: &str,
    weights: Option<Value>,
) {
    let mut request = json!({"Name": alias, "FunctionVersion": version});
    if let Some(weights) = weights {
        request["RoutingConfig"] = json!({"AdditionalVersionWeights": weights});
    }
    let (status, body) = emulator
        .lambda("POST", &format!("{FUNCTIONS_URI}/{function}/aliases"), request)
        .await;
    assert_eq!(
        status, 201,
        "case {function}:{alias}: CreateAlias must succeed, got {status} {body:?}"
    );
}

/// The alias as a client reads it: the version it invokes, and its routing
/// configuration.
async fn alias_of(emulator: &Emulator, function: &str, alias: &str) -> Value {
    let (status, body) = emulator
        .lambda(
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/aliases/{alias}"),
            Value::Null,
        )
        .await;
    assert_eq!(
        status, 200,
        "case {function}:{alias}: GetAlias must succeed, got {status} {body:?}"
    );
    body
}

/// An Lambda application with one deployment group on it, answering the group
/// name.
async fn given_lambda_group(emulator: &Emulator, application: &str) -> String {
    emulator
        .ok(
            "CreateApplication",
            json!({"applicationName": application, "computePlatform": "Lambda"}),
        )
        .await;
    let group = format!("{application}-dg");
    emulator
        .ok(
            "CreateDeploymentGroup",
            json!({
                "applicationName": application,
                "deploymentGroupName": group,
                "deploymentConfigName": "CodeDeployDefault.LambdaAllAtOnce",
                "serviceRoleArn": SERVICE_ROLE,
            }),
        )
        .await;
    group
}

/// The AppSpec of an Lambda deployment, YAML-formatted as the AWS CodeDeploy User
/// Guide writes one: one `Resources` entry naming the function, the alias and the
/// two versions traffic moves between.
fn app_spec_yaml(function: &str, alias: &str, current: &str, target: &str) -> String {
    format!(
        "version: 0.0\n\
         Resources:\n\
         \x20 - {function}:\n\
         \x20     Type: AWS::Lambda::Function\n\
         \x20     Properties:\n\
         \x20       Name: {function}\n\
         \x20       Alias: {alias}\n\
         \x20       CurrentVersion: \"{current}\"\n\
         \x20       TargetVersion: \"{target}\"\n"
    )
}

/// The same AppSpec in its other spelling: "the content is formatted as JSON or
/// YAML stored as a RawString"
/// (`smithy_query(query="shape",shape="RevisionLocation",member="appSpecContent")`).
fn app_spec_json(function: &str, alias: &str, current: &str, target: &str) -> String {
    json!({
        "version": 0.0,
        "Resources": [{
            function: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Name": function,
                    "Alias": alias,
                    "CurrentVersion": current,
                    "TargetVersion": target,
                },
            },
        }],
    })
    .to_string()
}

/// An `appSpecContent` revision carrying `content`.
fn app_spec_revision(content: &str) -> Value {
    json!({"revisionType": "AppSpecContent", "appSpecContent": {"content": content}})
}

/// AWS's fixed sentence for the `INVALID_LAMBDA_FUNCTION` error code, spelled out
/// here as a client matching on it reads it: the only variable part is the function
/// the AppSpec's `Resources` entry names, and the sentence deliberately says
/// neither which of the function, the version and the alias was missing nor what
/// the alias was called.
fn invalid_lambda_function_message(function: &str) -> String {
    format!(
        "The deployment failed because either the target Lambda function {function} does not \
         exist or the specified function version or alias cannot be found"
    )
}

/// A `string` revision carrying `content`: "the location of an Lambda deployment
/// revision stored as a RawString".
fn raw_string_revision(content: &str) -> Value {
    json!({"revisionType": "String", "string": {"content": content}})
}

/// AWS's fixed sentence for the `INVALID_LAMBDA_CONFIGURATION` error code, spelled
/// out here as a client matching on it reads it: it names neither the deployment
/// nor the content it was handed, and says that the AppSpec file is what could not
/// be read — which is how a client tells this cause from the others that share the
/// code.
const INVALID_LAMBDA_CONFIGURATION_MESSAGE: &str = "The deployment failed because the AppSpec \
     file that specifies the AWS Lambda deployment configuration is missing or has an invalid \
     format";

/// Create a deployment of `revision` and answer its minted id.
async fn deploy(emulator: &Emulator, application: &str, group: &str, revision: Value) -> String {
    let created = emulator
        .ok(
            "CreateDeployment",
            json!({
                "applicationName": application,
                "deploymentGroupName": group,
                "revision": revision,
            }),
        )
        .await;
    created["deploymentId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// One row of the shift table: the revision form it deploys and where the alias
/// has to end up.
struct Shift {
    /// What the row is about.
    case: &'static str,
    /// How many versions the function publishes before the deployment.
    versions: usize,
    /// The version the alias invokes, and the AppSpec's `CurrentVersion`.
    current: &'static str,
    /// The AppSpec's `TargetVersion`, which is where the alias has to end up.
    target: &'static str,
    /// The weighted routing the alias carries before the deployment, if any.
    weights: Option<&'static str>,
    /// Whether the AppSpec travels as `appSpecContent` (else as `string`), and
    /// whether it is written as JSON (else as YAML).
    app_spec_content: bool,
    /// See [`Shift::app_spec_content`].
    json: bool,
}

/// RULE: an Lambda deployment that reaches the terminal status `Succeeded` has
/// carried out the traffic shift its revision's AppSpec describes — the function's
/// named alias invokes the AppSpec's `TargetVersion` and splits its invocations
/// with no other version — whatever form that AppSpec arrives in.
#[tokio::test]
async fn a_succeeded_lambda_deployment_has_moved_the_alias_to_the_target_version() {
    // Rows differ along what the rule generalises over: which version is
    // targeted, which direction the shift goes, whether the alias was already
    // splitting traffic, which revision member carries the AppSpec, and which of
    // the two formats it is written in.
    let cases: &[Shift] = &[
        Shift {
            case: "a YAML AppSpec shifting to the next version",
            versions: 2,
            current: "1",
            target: "2",
            weights: None,
            app_spec_content: true,
            json: false,
        },
        Shift {
            case: "a YAML AppSpec shifting two versions ahead",
            versions: 3,
            current: "1",
            target: "3",
            weights: None,
            app_spec_content: true,
            json: false,
        },
        Shift {
            case: "a YAML AppSpec shifting back to an earlier version",
            versions: 3,
            current: "3",
            target: "1",
            weights: None,
            app_spec_content: true,
            json: false,
        },
        Shift {
            case: "a JSON AppSpec",
            versions: 2,
            current: "1",
            target: "2",
            weights: None,
            app_spec_content: true,
            json: true,
        },
        Shift {
            case: "an AppSpec carried as the revision's string content",
            versions: 2,
            current: "1",
            target: "2",
            weights: None,
            app_spec_content: false,
            json: false,
        },
        Shift {
            case: "an alias that was already splitting its invocations",
            versions: 3,
            current: "1",
            target: "3",
            weights: Some("2"),
            app_spec_content: true,
            json: false,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let emulator = Emulator::with_lambda();
        let application = format!("shift-app-{index:02}");
        let function = format!("handler{index:02}");
        let alias = "live";
        let group = given_lambda_group(&emulator, &application).await;
        given_function(&emulator, &function, case.versions).await;
        given_alias(
            &emulator,
            &function,
            alias,
            case.current,
            case.weights
                .map(|version| json!({version.to_string(): 0.1})),
        )
        .await;

        let content = if case.json {
            app_spec_json(&function, alias, case.current, case.target)
        } else {
            app_spec_yaml(&function, alias, case.current, case.target)
        };
        let revision = if case.app_spec_content {
            app_spec_revision(&content)
        } else {
            raw_string_revision(&content)
        };
        let deployment_id = deploy(&emulator, &application, &group, revision).await;

        let info = terminal_deployment(&emulator, &deployment_id, case.case).await;
        assert_eq!(
            info["status"].as_str(),
            Some("Succeeded"),
            "case {:?}: expected the deployment to succeed, got {info:?}",
            case.case
        );

        let moved = alias_of(&emulator, &function, alias).await;
        assert_eq!(
            moved["FunctionVersion"].as_str(),
            Some(case.target),
            "case {:?}: expected the alias at the AppSpec's TargetVersion {}, got {moved:?}",
            case.case,
            case.target
        );
        assert!(
            moved["RoutingConfig"].is_null(),
            "case {:?}: expected no weighted routing left on the alias, got {moved:?}",
            case.case
        );
    }
}

/// RULE: a Lambda deployment performs the shift its revision's AppSpec DECLARES,
/// so a revision that declares none leaves it nothing to perform: it moves no
/// alias and ends `Failed` with the `INVALID_LAMBDA_CONFIGURATION` error
/// information rather than reporting success for a shift nothing carried out —
/// while a revision that declares one moves exactly the alias it names, and no
/// other alias of the function is touched.
#[tokio::test]
async fn a_revision_that_declares_no_shift_moves_no_alias() {
    let emulator = Emulator::with_lambda();
    let group = given_lambda_group(&emulator, "undeclared").await;
    given_function(&emulator, "handler", 2).await;
    given_alias(&emulator, "handler", "live", "1", None).await;
    // A second alias of the same function, which the AppSpec never names.
    given_alias(&emulator, "handler", "canary", "1", None).await;

    // Rows differ along what the revision carries: an AppSpec with no resources
    // at all, one whose resource is of another compute platform, one whose Lambda
    // resource states no properties, content that is no document, and an AppSpec
    // naming the OTHER alias (which moves instead). Only the last declares a
    // shift, so only the last can succeed.
    let cases: &[(&str, Value, &str, &str, &str)] = &[
        (
            "an AppSpec that declares no resource",
            app_spec_revision("version: 0.0"),
            "1",
            "1",
            "Failed",
        ),
        (
            "an AppSpec of another compute platform",
            app_spec_revision(
                "version: 0.0\nResources:\n  - web:\n      Type: AWS::ECS::Service\n",
            ),
            "1",
            "1",
            "Failed",
        ),
        (
            "an AppSpec whose Lambda resource states no properties",
            app_spec_revision(
                "version: 0.0\nResources:\n  - handler:\n      Type: AWS::Lambda::Function\n",
            ),
            "1",
            "1",
            "Failed",
        ),
        (
            "content that is no document at all",
            app_spec_revision("\tnot: a document\n"),
            "1",
            "1",
            "Failed",
        ),
        (
            "an AppSpec naming the other alias",
            app_spec_revision(&app_spec_yaml("handler", "canary", "1", "2")),
            "1",
            "2",
            "Succeeded",
        ),
    ];

    for (case, revision, live, canary, expected) in cases {
        let deployment_id = deploy(&emulator, "undeclared", &group, revision.clone()).await;
        let info = terminal_deployment(&emulator, &deployment_id, case).await;
        assert_eq!(
            info["status"].as_str(),
            Some(*expected),
            "case {case:?}: expected the deployment to end {expected}, got {info:?}"
        );
        if *expected == "Failed" {
            assert_eq!(
                info["errorInformation"]["code"].as_str(),
                Some("INVALID_LAMBDA_CONFIGURATION"),
                "case {case:?}: expected the INVALID_LAMBDA_CONFIGURATION code, got {info:?}"
            );
            assert_eq!(
                info["errorInformation"]["message"].as_str(),
                Some(INVALID_LAMBDA_CONFIGURATION_MESSAGE),
                "case {case:?}: expected that failure's own sentence, got {:?}",
                info["errorInformation"]["message"]
            );
        }
        for (alias, expected) in [("live", live), ("canary", canary)] {
            let read = alias_of(&emulator, "handler", alias).await;
            assert_eq!(
                read["FunctionVersion"].as_str(),
                Some(*expected),
                "case {case:?}: expected the {alias} alias at version {expected}, got {read:?}"
            );
        }
    }
}

/// RULE: a deployment whose declared shift cannot be carried out reports no
/// success for it: it ends `Failed`, with the `INVALID_LAMBDA_FUNCTION` error
/// information naming the FUNCTION its AppSpec asked for — one fixed sentence for
/// all three missing resources, which deliberately does not say which of them was
/// missing — and no alias moves.
#[tokio::test]
async fn a_shift_that_cannot_be_carried_out_fails_the_deployment() {
    let emulator = Emulator::with_lambda();
    let group = given_lambda_group(&emulator, "denied").await;
    given_function(&emulator, "handler", 2).await;
    given_alias(&emulator, "handler", "live", "1", None).await;

    // Rows differ along which part of the AppSpec names something AWS Lambda does
    // not hold: the function, the alias, or the version traffic would move to. All
    // three earn the same sentence, naming only the function.
    let cases: &[(&str, &str, &str, &str, String)] = &[
        (
            "a function nothing created",
            "absent",
            "live",
            "2",
            invalid_lambda_function_message("absent"),
        ),
        (
            "an alias the function does not hold",
            "handler",
            "blue",
            "2",
            invalid_lambda_function_message("handler"),
        ),
        (
            "a target version the function never published",
            "handler",
            "live",
            "9",
            invalid_lambda_function_message("handler"),
        ),
    ];

    for (case, function, alias, target, message) in cases {
        let revision = app_spec_revision(&app_spec_yaml(function, alias, "1", target));
        let deployment_id = deploy(&emulator, "denied", &group, revision).await;
        let info = terminal_deployment(&emulator, &deployment_id, case).await;
        assert_eq!(
            (
                info["status"].as_str(),
                info["errorInformation"]["code"].as_str(),
                info["errorInformation"]["message"].as_str(),
            ),
            (
                Some("Failed"),
                Some("INVALID_LAMBDA_FUNCTION"),
                Some(message.as_str()),
            ),
            "case {case:?}: expected a Failed deployment naming the missing resource, got {info:?}"
        );
        let read = alias_of(&emulator, "handler", "live").await;
        assert_eq!(
            read["FunctionVersion"].as_str(),
            Some("1"),
            "case {case:?}: expected the alias left where it was, got {read:?}"
        );
    }
}

/// RULE: the shift happens where a client reads it — in AWS Lambda — so a
/// deployment into an emulator with no Lambda beside it cannot carry it out, and
/// says so rather than reporting success.
#[tokio::test]
async fn a_shift_with_no_lambda_beside_the_service_fails_the_deployment() {
    let emulator = Emulator::new();
    let group = given_lambda_group(&emulator, "unreachable").await;
    let revision = app_spec_revision(&app_spec_yaml("handler", "live", "1", "2"));
    let deployment_id = deploy(&emulator, "unreachable", &group, revision).await;

    let case = "a deployment with no Lambda to shift traffic in";
    let info = terminal_deployment(&emulator, &deployment_id, case).await;
    assert_eq!(
        (
            info["status"].as_str(),
            info["errorInformation"]["code"].as_str(),
        ),
        (Some("Failed"), Some("INVALID_LAMBDA_FUNCTION")),
        "case {case:?}: expected a Failed deployment, got {info:?}"
    );
    // The refusal is the deployment's own outcome, not a refusal of the call that
    // created it: `CreateDeployment` answered a deployment id.
    assert!(
        deployment_id.starts_with("d-"),
        "case {case:?}: expected the deployment to have been accepted, got {deployment_id:?}"
    );
}

/// The ARN AWS Lambda mints for a function of `name` in the Region this suite's
/// requests are scoped to, spelled off the ARN it minted for a function it DID
/// create: only the resource name differs, so no ARN is written by hand here.
fn sibling_function_arn(created: &str, name: &str) -> String {
    let (prefix, _) = created
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("the created function's ARN {created:?} names a resource"));
    format!("{prefix}:{name}")
}

/// The sentence AWS Lambda's READ of an alias answers when the function holds no
/// alias of that name: a fixed lead-in and the fully-qualified alias ARN the
/// request asked about, which is the function's own ARN qualified by the alias
/// name.
fn alias_not_found_message(function_arn: &str, alias: &str) -> String {
    format!("Cannot find alias arn: {function_arn}:{alias}")
}

/// The sentence AWS Lambda answers when the request names no function it holds,
/// naming the ARN the request resolved to.
fn function_not_found_message(function_arn: &str) -> String {
    format!("Function not found: {function_arn}")
}

/// What AWS Lambda answers for the read of one alias: the HTTP status and the
/// message of the body, under either casing the JSON envelopes spell it in.
async fn alias_read(emulator: &Emulator, function: &str, alias: &str) -> (u16, String) {
    let (status, body) = emulator
        .lambda(
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/aliases/{alias}"),
            Value::Null,
        )
        .await;
    let message = body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_default()
        .to_string();
    (status, message)
}

/// The names of the aliases AWS Lambda holds for `function`, as `ListAliases`
/// reports them — the collection an alias that was never created must stay out of.
async fn alias_names(emulator: &Emulator, function: &str) -> Vec<String> {
    let (status, body) = emulator
        .lambda(
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/aliases"),
            Value::Null,
        )
        .await;
    assert_eq!(
        status, 200,
        "ListAliases for {function} must succeed, got {status} {body:?}"
    );
    body["Aliases"]
        .as_array()
        .map(|aliases| {
            aliases
                .iter()
                .filter_map(|alias| alias["Name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// One row of the unresolved-reference table: what the AppSpec POINTS AT that AWS
/// Lambda does not hold.
struct Reference {
    /// What the row is about.
    case: &'static str,
    /// The function the AppSpec's `Name` property names.
    function: &'static str,
    /// The alias its `Alias` property names, which no `CreateAlias` ever created.
    alias: &'static str,
    /// Its `TargetVersion`.
    target: &'static str,
    /// Whether AWS Lambda holds the function the row names, which is what decides
    /// which absence its read of that alias reports.
    function_exists: bool,
}

/// RULE: the Lambda resources an AppSpec names are RESOLVED, never created — a
/// deployment whose declared shift names a function, an alias or a version AWS
/// Lambda does not hold reports the failure as its own terminal outcome and creates
/// none of them, so the alias it merely named is still absent afterwards and every
/// alias the function does hold still invokes the version, and splits its
/// invocations the way, it did before.
#[tokio::test]
async fn an_unresolved_appspec_reference_creates_nothing_in_lambda() {
    let emulator = Emulator::with_lambda();
    let group = given_lambda_group(&emulator, "referenced").await;
    let function_arn = given_function(&emulator, "handler", 3).await;
    // The one alias the function DOES hold, which no row's AppSpec names: it
    // carries weighted routing of its own, so a failed deployment that cleared it
    // would show up here.
    given_alias(
        &emulator,
        "handler",
        "live",
        "2",
        Some(json!({"3": 0.25})),
    )
    .await;

    // Rows differ along which referent is missing and along how the missing alias
    // name is spelled — the model's `Alias` admits letters, digits, '-' and '_'
    // (`smithy_query(query="shape",shape="Alias")` of the Lambda model) — and the
    // last row's function is missing as well, so the absence Lambda reports for
    // the read differs too.
    let cases: &[Reference] = &[
        Reference {
            case: "an alias nothing created",
            function: "handler",
            alias: "ghost",
            target: "3",
            function_exists: true,
        },
        Reference {
            case: "an alias whose name carries a hyphen",
            function: "handler",
            alias: "ghost-two",
            target: "3",
            function_exists: true,
        },
        Reference {
            case: "an alias whose name mixes case with an underscore",
            function: "handler",
            alias: "Ghost_three",
            target: "3",
            function_exists: true,
        },
        Reference {
            case: "an alias nothing created, targeting a version nothing published",
            function: "handler",
            alias: "ghost4",
            target: "9",
            function_exists: true,
        },
        Reference {
            case: "an alias of a function nothing created",
            function: "absent",
            alias: "ghost5",
            target: "3",
            function_exists: false,
        },
    ];

    for case in cases {
        let revision = app_spec_revision(&app_spec_yaml(
            case.function,
            case.alias,
            "2",
            case.target,
        ));
        let deployment_id = deploy(&emulator, "referenced", &group, revision).await;
        // The verdict is the deployment's own terminal state, not a refusal of the
        // call that created it.
        assert!(
            deployment_id.starts_with("d-"),
            "case {:?}: expected the deployment to have been accepted, got {deployment_id:?}",
            case.case
        );
        let info = terminal_deployment(&emulator, &deployment_id, case.case).await;
        let expected_message = invalid_lambda_function_message(case.function);
        assert_eq!(
            (
                info["status"].as_str(),
                info["errorInformation"]["code"].as_str(),
                info["errorInformation"]["message"].as_str(),
            ),
            (
                Some("Failed"),
                Some("INVALID_LAMBDA_FUNCTION"),
                Some(expected_message.as_str()),
            ),
            "case {:?}: expected a Failed deployment carrying the INVALID_LAMBDA_FUNCTION \
             information, got {info:?}",
            case.case
        );

        // Effect absent: the alias the AppSpec merely named is still not there, so
        // the read of it answers the same absence it answered before the
        // deployment.
        let expected_absence = if case.function_exists {
            alias_not_found_message(&function_arn, case.alias)
        } else {
            function_not_found_message(&sibling_function_arn(&function_arn, case.function))
        };
        let (status, message) = alias_read(&emulator, case.function, case.alias).await;
        assert_eq!(
            (status, message.as_str()),
            (404, expected_absence.as_str()),
            "case {:?}: expected the named alias to be absent with {expected_absence:?}, got \
             {status} {message:?}",
            case.case
        );

        // Effect absent: the alias the AppSpec did not name is untouched — same
        // version, same weighted routing.
        let untouched = alias_of(&emulator, "handler", "live").await;
        assert_eq!(
            (
                untouched["FunctionVersion"].as_str(),
                &untouched["RoutingConfig"]["AdditionalVersionWeights"],
            ),
            (Some("2"), &json!({"3": 0.25})),
            "case {:?}: expected the unnamed alias left as it was, got {untouched:?}",
            case.case
        );
    }

    // And nothing was added to the collection either: the function holds exactly
    // the alias that was created through `CreateAlias`.
    let held = alias_names(&emulator, "handler").await;
    assert_eq!(
        held,
        vec!["live".to_string()],
        "expected the function to hold only the alias that was created, got {held:?}"
    );
}

/// RULE: whether the alias an AppSpec names EXISTS — account state, not the
/// document's shape — is what decides the deployment's outcome: one AppSpec,
/// differing in nothing but that name, fails against a name nothing created and
/// succeeds against one that was created, moving it to the AppSpec's
/// `TargetVersion`.
#[tokio::test]
async fn alias_existence_alone_decides_the_outcome() {
    let emulator = Emulator::with_lambda();
    let group = given_lambda_group(&emulator, "existence").await;
    let function_arn = given_function(&emulator, "handler", 3).await;
    given_alias(&emulator, "handler", "live", "2", None).await;

    // Rows differ only in the alias name inside one AppSpec, and in whether AWS
    // Lambda holds it: the last row repeats the missing name AFTER a shift that
    // worked, so a deployment that succeeded cannot have made it resolvable.
    let cases: &[(&str, &str, bool)] = &[
        ("a name nothing created", "ghost", false),
        ("the name that was created", "live", true),
        (
            "a name nothing created, after a shift that worked",
            "ghost",
            false,
        ),
    ];

    for (case, alias, exists) in cases {
        let revision = app_spec_revision(&app_spec_yaml("handler", alias, "2", "3"));
        let deployment_id = deploy(&emulator, "existence", &group, revision).await;
        let info = terminal_deployment(&emulator, &deployment_id, case).await;
        let expected_status = if *exists { "Succeeded" } else { "Failed" };
        assert_eq!(
            info["status"].as_str(),
            Some(expected_status),
            "case {case:?}: expected the deployment to end {expected_status}, got {info:?}"
        );
        if *exists {
            let moved = alias_of(&emulator, "handler", alias).await;
            assert_eq!(
                moved["FunctionVersion"].as_str(),
                Some("3"),
                "case {case:?}: expected the alias at the AppSpec's TargetVersion, got {moved:?}"
            );
        } else {
            let expected_absence = alias_not_found_message(&function_arn, alias);
            let (status, message) = alias_read(&emulator, "handler", alias).await;
            assert_eq!(
                (status, message.as_str()),
                (404, expected_absence.as_str()),
                "case {case:?}: expected the named alias to be absent with \
                 {expected_absence:?}, got {status} {message:?}"
            );
        }
    }
}
