// SPDX-License-Identifier: CC0-1.0

//! WHERE the deep link an action's state carries is announced.
//!
//! The rule under test: `ActionState$entityUrl` is the action type's published
//! `entityUrlTemplate` with every `{Config:<property>}` reference replaced by the action's
//! configuration value for that property, published on the address the READING request
//! reached this emulator at — so a caller can follow it — and an action type publishing no
//! template reports no link at all.
//!
//! The rows span what the rule generalises over: the action type (a source, an invoke, a
//! deploy, an approval gate that publishes no template, and ones the CALLER registered,
//! whose template names its system in each of the three forms a URL reference can — a
//! scheme and a host, a host alone, and neither), the configuration values its references
//! resolve to, the Region the state is read in, and the endpoint the reading request
//! addressed (an address with a port, a loopback name, a bracketed IPv6 literal, an
//! in-process caller on a served emulator, and an emulator served on no port at all).
//!
//! Every assertion goes through the router, and every template is read back out of
//! `ListActionTypes` rather than written out here: what is asserted is that the state
//! resolves the PUBLISHED template onto this emulator's own address, not that the link
//! equals one particular string.

use serde_json::{json, Value};

use crate::pipeline_lifecycle::{build_stage, source_stage, Emulator, ROLE_ARN};

/// The base this emulator publishes for itself when it is served on no port at all —
/// a host nothing resolves, which is the truth about such an emulator.
const UNSERVED_AUTHORITY: &str = "emulator.invalid";

/// Hosts a reported link may never name: this emulator neither serves them nor is reached
/// on them, so a caller following a link there leaves the emulated environment.
const FOREIGN_HOSTS: [&str; 4] = [
    "console.aws.amazon.com",
    "amazonaws.com",
    "ci.example.test",
    "github.com",
];

/// One row of the table.
struct Row {
    case: &'static str,
    /// The base URL the emulator publishes, when it is served on a port.
    base: Option<&'static str>,
    /// The endpoint the reading request addressed.
    host: Option<&'static str>,
    /// The Region every call of the row is signed for.
    region: &'static str,
    /// The `ActionTypeId` of the action under test, and the owner filter the listing
    /// reports it under.
    category: &'static str,
    owner: &'static str,
    provider: &'static str,
    /// The action's own configuration — what the template's references resolve to.
    configuration: Value,
    /// Whether the action takes the source stage's artifact (its action type's
    /// `inputArtifactDetails` minimum).
    input_artifact: bool,
    /// The `entityUrlTemplate` to REGISTER the action type with, for a row whose owner is
    /// `Custom` — the template text is the caller's, so the forms a `UrlTemplate` may take
    /// are a row's to vary. `None` for an action type the service publishes itself.
    registered_template: Option<&'static str>,
    /// The `host[:port]` the reported link must carry.
    authority: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a CodeCommit source read on the loopback data port",
            base: None,
            host: Some("127.0.0.1:37915"),
            region: "us-west-2",
            category: "Source",
            owner: "AWS",
            provider: "CodeCommit",
            configuration: json!({"RepositoryName": "etl-repo", "BranchName": "main"}),
            input_artifact: false,
            registered_template: None,
            authority: "127.0.0.1:37915",
        },
        Row {
            case: "a Lambda invoke read through a loopback name",
            base: None,
            host: Some("localhost:9297"),
            region: "us-east-1",
            category: "Invoke",
            owner: "AWS",
            provider: "Lambda",
            configuration: json!({"FunctionName": "transform", "UserParameters": "{}"}),
            input_artifact: false,
            registered_template: None,
            authority: "localhost:9297",
        },
        // An action type the issue this suite was written for did not cite, whose template
        // carries its reference in a path segment and the Region in the query.
        Row {
            case: "an S3 deploy read over an IPv6 loopback",
            base: None,
            host: Some("[::1]:9297"),
            region: "eu-west-2",
            category: "Deploy",
            owner: "AWS",
            provider: "S3",
            configuration: json!({
                "BucketName": "deploy-target",
                "Extract": "true",
                "ObjectKey": "app/release.zip",
            }),
            input_artifact: true,
            registered_template: None,
            authority: "[::1]:9297",
        },
        Row {
            case: "a Step Functions invoke on an emulator served at its data port",
            base: Some("https://127.0.0.1:9443"),
            host: None,
            region: "ap-southeast-1",
            category: "Invoke",
            owner: "AWS",
            provider: "StepFunctions",
            configuration: json!({
                "StateMachineArn": "arn:aws:states:ap-southeast-1:000000000000:stateMachine:etl",
            }),
            input_artifact: false,
            registered_template: None,
            authority: "127.0.0.1:9443",
        },
        Row {
            case: "an approval gate, whose action type publishes no template",
            base: None,
            host: Some("127.0.0.1:41001"),
            region: "us-east-1",
            category: "Approval",
            owner: "AWS",
            provider: "Manual",
            configuration: json!({"CustomData": "ship it"}),
            input_artifact: false,
            registered_template: None,
            authority: "127.0.0.1:41001",
        },
        // The three forms a registered `UrlTemplate` can name its system in, which is
        // what decides how much of the template is the HOST (RFC 3986): a scheme and an
        // authority, an authority alone, and a reference with neither — which names a
        // path, so all of it is the resource.
        Row {
            case: "a registered template naming a scheme and a host, served on no port",
            base: None,
            host: None,
            region: "us-east-2",
            category: "Test",
            owner: "Custom",
            provider: "Nightly-Suite_1",
            configuration: json!({"JobName": "nightly"}),
            input_artifact: false,
            registered_template: Some("https://ci.example.test/jobs/{Config:JobName}/runs"),
            authority: UNSERVED_AUTHORITY,
        },
        Row {
            case: "a registered template naming a host and no scheme",
            base: None,
            host: Some("127.0.0.1:37915"),
            region: "us-east-2",
            category: "Test",
            owner: "Custom",
            provider: "Nightly-Suite_2",
            configuration: json!({"JobName": "weekly"}),
            input_artifact: false,
            registered_template: Some("//ci.example.test/jobs/{Config:JobName}/runs"),
            authority: "127.0.0.1:37915",
        },
        Row {
            case: "a registered template naming a path and no host at all",
            base: None,
            host: Some("localhost:9297"),
            region: "us-east-2",
            category: "Test",
            owner: "Custom",
            provider: "Nightly-Suite_3",
            configuration: json!({"JobName": "hourly"}),
            input_artifact: false,
            registered_template: Some("jobs/{Config:JobName}/runs"),
            authority: "localhost:9297",
        },
        // A path reference whose QUERY carries a `://` of its own: the text before it is no
        // scheme, so none of the reference is a host and all of it is the resource.
        Row {
            case: "a registered template carrying a scheme separator inside its query",
            base: None,
            host: Some("127.0.0.1:41001"),
            region: "us-east-2",
            category: "Test",
            owner: "Custom",
            provider: "Nightly-Suite_4",
            configuration: json!({"JobName": "nightly-2"}),
            input_artifact: false,
            registered_template: Some("jobs/{Config:JobName}?callback=custom://run"),
            authority: "127.0.0.1:41001",
        },
    ]
}

/// The action under test, as a declared action of its own stage.
fn action_of(row: &Row) -> Value {
    let mut action = json!({
        "name": "Subject",
        "actionTypeId": {
            "category": row.category,
            "owner": row.owner,
            "provider": row.provider,
            "version": "1",
        },
        "configuration": row.configuration,
    });
    if row.input_artifact {
        action["inputArtifacts"] = json!([{"name": "SourceOut"}]);
    }
    if row.category == "Source" {
        action["outputArtifacts"] = json!([{"name": "SourceOut"}]);
    }
    action
}

/// The declaration the row's action is read out of, and the stage the state reports it
/// under: a source action is the pipeline's own source stage, and any other action acts on
/// what a source stage before it fetched (`state::validate_declaration` holds a
/// declaration to both).
fn declaration_of(row: &Row, name: &str) -> (Value, usize) {
    let (stages, stage_index) = if row.category == "Source" {
        (
            vec![
                json!({"name": "Source", "actions": [action_of(row)]}),
                build_stage("Build", 1),
            ],
            0,
        )
    } else {
        (
            vec![
                source_stage("Source"),
                json!({"name": "Subject", "actions": [action_of(row)]}),
            ],
            1,
        )
    };
    (
        json!({
            "name": name,
            "roleArn": ROLE_ARN,
            "artifactStore": {"type": "S3", "location": "my-bucket"},
            "stages": stages,
        }),
        stage_index,
    )
}

/// The part of `url` that names the RESOURCE, by RFC 3986's reading of the reference the
/// template wrote: a reference carrying a scheme (`https://host/path`) or an authority
/// alone (`//host/path`) names a HOST, so the resource is everything from the first `/`,
/// `?` or `#` after it; a reference carrying neither names a PATH, so all of it is the
/// resource — and text before a `://` is a scheme only if it is one
/// (`ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`), so a `://` inside a path or query names
/// no host.
fn resource_of(url: &str) -> &str {
    match url
        .split_once("://")
        .filter(|(scheme, _)| names_a_scheme(scheme))
    {
        Some((_, after_scheme)) => after_authority(after_scheme),
        None => match url.strip_prefix("//") {
            Some(after_prefix) => after_authority(after_prefix),
            None => url,
        },
    }
}

/// Whether the text before a `://` is RFC 3986's `scheme` production.
fn names_a_scheme(scheme: &str) -> bool {
    let mut characters = scheme.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && characters.all(|next| next.is_ascii_alphanumeric() || matches!(next, '+' | '-' | '.'))
}

/// The resource part of a reference whose leading text is an authority.
fn after_authority(reference: &str) -> &str {
    reference
        .find(['/', '?', '#'])
        .map_or("/", |start| &reference[start..])
}

/// The link this emulator announces on `authority` for the resolved template `resolved`:
/// the resource it names, under this emulator's own address.
fn announced(authority: &str, resolved: &str) -> String {
    let resource = resource_of(resolved);
    let separator = if resource.starts_with(['/', '?', '#']) {
        ""
    } else {
        "/"
    };
    format!("https://{authority}{separator}{resource}")
}

/// `template` with every `{Config:<property>}` reference replaced by `configuration`'s
/// value for that property.
fn resolved(template: &str, configuration: &Value) -> String {
    let mut link = template.to_string();
    for (property, value) in configuration.as_object().into_iter().flatten() {
        let value = value.as_str().unwrap_or_default();
        link = link.replace(&format!("{{Config:{property}}}"), value);
    }
    link
}

/// The `entityUrlTemplate` the listing publishes for one action type in `region`, or
/// `None` when that action type publishes none.
async fn published_template(emulator: &Emulator, row: &Row) -> Option<String> {
    let catalog = emulator
        .ok_addressed(
            row.region,
            row.host,
            "ListActionTypes",
            &json!({"actionOwnerFilter": row.owner}),
        )
        .await;
    catalog["actionTypes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| {
            entry["id"]["category"] == json!(row.category)
                && entry["id"]["provider"] == json!(row.provider)
        })
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: the listing publishes the action type under test, got {catalog}",
                row.case
            )
        })["settings"]["entityUrlTemplate"]
        .as_str()
        .map(str::to_string)
}

/// Register the row's action type when the CALLER owns it, with the template the row
/// names — the registration whose link this emulator still has to announce on an address
/// a caller can reach.
async fn register_if_custom(emulator: &Emulator, row: &Row) {
    let Some(template) = row.registered_template else {
        return;
    };
    emulator
        .ok_addressed(
            row.region,
            row.host,
            "CreateCustomActionType",
            &json!({
                "category": row.category,
                "provider": row.provider,
                "version": "1",
                "settings": {
                    "entityUrlTemplate": template,
                },
                "configurationProperties": [{
                    "name": "JobName",
                    "required": true,
                    "key": true,
                    "secret": false,
                    "queryable": false,
                }],
                "inputArtifactDetails": {"minimumCount": 0, "maximumCount": 1},
                "outputArtifactDetails": {"minimumCount": 0, "maximumCount": 1},
            }),
        )
        .await;
}

#[tokio::test]
async fn an_action_state_links_its_resource_on_the_address_this_emulator_is_reached_at() {
    for (index, row) in rows().into_iter().enumerate() {
        let emulator = Emulator::served_at(row.base);
        register_if_custom(&emulator, &row).await;

        let pipeline = format!("linked-{index}");
        let (declaration, stage_index) = declaration_of(&row, &pipeline);
        emulator
            .ok_addressed(
                row.region,
                row.host,
                "CreatePipeline",
                &json!({"pipeline": declaration}),
            )
            .await;

        let template = published_template(&emulator, &row).await;
        let state = emulator
            .ok_addressed(
                row.region,
                row.host,
                "GetPipelineState",
                &json!({"name": pipeline}),
            )
            .await;
        let action = &state["stageStates"][stage_index]["actionStates"][0];
        let reported = action.get("entityUrl").and_then(Value::as_str);

        let Some(template) = template else {
            assert_eq!(
                reported, None,
                "case {:?}: the action type publishes no template, so the state reports no \
                 link, got {action}",
                row.case
            );
            continue;
        };

        let link = resolved(&template, &row.configuration);
        assert!(
            !link.contains("{Config:"),
            "case {:?}: every reference of the published template is resolved by the \
             action's configuration, got {link:?}",
            row.case
        );
        let expected = announced(row.authority, &link);
        assert_eq!(
            reported,
            Some(expected.as_str()),
            "case {:?}: expected the resource the action's configuration names, announced \
             on the address the read came in on ({expected:?}), got {reported:?}",
            row.case
        );
        for foreign in FOREIGN_HOSTS {
            assert!(
                !expected.contains(foreign),
                "case {:?}: the reported link names the host {foreign:?}, which this \
                 emulator does not serve: {expected:?}",
                row.case
            );
        }
    }
}

/// RULE (the same rule as an invariant): the link reported for ONE stored action names the
/// same resource whichever endpoint it is read at, and is announced on that endpoint — so
/// the authority follows the reader and the resource does not.
#[tokio::test]
async fn one_stored_action_is_linked_on_each_endpoint_it_is_read_at() {
    // The endpoints the same pipeline is read at, each with the authority its links carry.
    let endpoints: [(Option<&str>, &str); 4] = [
        (Some("127.0.0.1:37915"), "127.0.0.1:37915"),
        (Some("localhost:9297"), "localhost:9297"),
        (Some("[::1]:9297"), "[::1]:9297"),
        (None, UNSERVED_AUTHORITY),
    ];
    let emulator = Emulator::served_at(None);
    let row = Row {
        case: "one pipeline read at several endpoints",
        base: None,
        host: None,
        region: "us-west-2",
        category: "Deploy",
        owner: "AWS",
        provider: "S3",
        configuration: json!({"BucketName": "deploy-target", "Extract": "true"}),
        input_artifact: true,
        registered_template: None,
        authority: UNSERVED_AUTHORITY,
    };
    let (declaration, stage_index) = declaration_of(&row, "read-anywhere");
    emulator
        .ok_addressed(
            row.region,
            None,
            "CreatePipeline",
            &json!({"pipeline": declaration}),
        )
        .await;
    let template = published_template(&emulator, &row)
        .await
        .unwrap_or_else(|| panic!("the S3 deploy action type publishes a template"));
    let resolved_template = resolved(&template, &row.configuration);

    for (host, authority) in endpoints {
        let state = emulator
            .ok_addressed(
                row.region,
                host,
                "GetPipelineState",
                &json!({"name": "read-anywhere"}),
            )
            .await;
        let reported = state["stageStates"][stage_index]["actionStates"][0]["entityUrl"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            reported,
            announced(authority, &resolved_template),
            "case {host:?}: expected the stored action's resource announced on the endpoint \
             the read came in on, got {reported:?}"
        );
    }
}
