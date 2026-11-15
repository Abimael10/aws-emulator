// SPDX-License-Identifier: CC0-1.0

//! The per-entry DETAIL of the action-type catalog the service publishes itself, over the
//! wire it actually speaks: `awsJson1_1`, `POST /` with
//! `X-Amz-Target: CodePipeline_20150709.ListActionTypes`.
//!
//! The rule under test: an action type the SERVICE publishes reports its provider's own
//! catalog detail — every configuration parameter the provider documents, each as a key
//! carrying the text a console displays for it and the secret flag of a credential the
//! caller hands over, the provider's documented input- and output-artifact bounds, and
//! the deep links the provider publishes, written for the Region the catalog is read for.
//!
//! Every read goes through the router, so nothing here can pass by reading a handler's
//! internals.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_codepipeline_codepipeline_20150709::state::{
    BuiltInActionType, BUILT_IN_ACTION_TYPES, CATALOG_REGION_PLACEHOLDER,
};

/// The `X-Amz-Target` prefix OPERATIONS.md records for this service.
const TARGET_PREFIX: &str = "CodePipeline_20150709";

/// `Url` / `UrlTemplate`'s modelled `@length(min: 1, max: 2048)`, the shapes every
/// `ActionTypeSettings` member targets
/// (`smithy_query query="shape" shape="ActionTypeSettings"`).
const URL_LENGTH: (usize, usize) = (1, 2_048);

/// One emulator: the shared engine, the service mounted on it, and the router that
/// dispatches to it — exactly how `core/router` assembles the production emulator.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_codepipeline_codepipeline_20150709::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// Every action type `ListActionTypes` reports for `body`, paged to the end, called
    /// signed for `region` the way a real SDK request carries it.
    async fn listed(&self, region: &str, body: &Value) -> Vec<Value> {
        let mut all = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut body = body.clone();
            if let Some(token) = &token {
                body["nextToken"] = json!(token);
            }
            let authorization = format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/codepipeline/\
                 aws4_request, SignedHeaders=host;x-amz-date, Signature=0123456789abcdef"
            );
            let request = http::Request::builder()
                .method("POST")
                .uri("/")
                .header("content-type", "application/x-amz-json-1.1")
                .header("x-amz-target", format!("{TARGET_PREFIX}.ListActionTypes"))
                .header("authorization", authorization)
                .body(bytes::Bytes::from(body.to_string()))
                .expect("a valid request");
            let response = self.router.dispatch(request).await;
            let status = response.status().as_u16();
            let answer: Value = serde_json::from_slice(&response.into_body())
                .unwrap_or_else(|error| panic!("ListActionTypes {body}: not JSON ({error})"));
            assert_eq!(
                status, 200,
                "ListActionTypes {body} in {region}: expected HTTP 200, got {status} with \
                 {answer}"
            );
            all.extend(
                answer["actionTypes"]
                    .as_array()
                    .unwrap_or_else(|| panic!("actionTypes is a list, got {answer}"))
                    .clone(),
            );
            match answer.get("nextToken").and_then(Value::as_str) {
                Some(next) => token = Some(next.to_string()),
                None => break all,
            }
        }
    }

    /// The one entry the catalog reports for `(category, owner, provider, version)` when
    /// asked with `body`.
    async fn entry(&self, region: &str, body: &Value, id: (&str, &str, &str, &str)) -> Value {
        let listed = self.listed(region, body).await;
        let found: Vec<&Value> = listed
            .iter()
            .filter(|entry| {
                entry["id"]["category"] == json!(id.0)
                    && entry["id"]["owner"] == json!(id.1)
                    && entry["id"]["provider"] == json!(id.2)
                    && entry["id"]["version"] == json!(id.3)
            })
            .collect();
        assert_eq!(
            found.len(),
            1,
            "case {id:?}: the catalog reports this action type exactly once, got \
             {found:?} of {listed:?}"
        );
        found[0].clone()
    }
}

/// One configuration property as the catalog reports it: the wire form of
/// `ActionConfigurationProperty` for a property of an action type the service publishes —
/// always a key, never queryable, always described.
fn described(name: &str, description: &str, required: bool, secret: bool) -> Value {
    json!({
        "name": name,
        "required": required,
        "key": true,
        "secret": secret,
        "queryable": false,
        "description": description,
    })
}

/// `(minimumCount, maximumCount)` as `ArtifactDetails` reports it.
fn artifacts(minimum: i64, maximum: i64) -> Value {
    json!({"minimumCount": minimum, "maximumCount": maximum})
}

/// One row of [`catalog_detail`]: the identity of a published action type and the whole
/// `ActionType` the catalog must report for it in `region`.
struct Row {
    case: &'static str,
    id: (&'static str, &'static str, &'static str, &'static str),
    entry: Value,
}

/// The catalog detail of a SET of published action types, in `region`.
///
/// The rows differ along everything the rule generalises over: the category, the owner,
/// how many parameters the provider documents and whether one of them is a secret, both
/// artifact bounds, which `settings` members the provider publishes, whether its deep
/// link substitutes a configuration property, and whether its links name a Region at all.
/// Two of them are entries this issue's report did not cite (`Approval/AWS/Manual`,
/// `Build/AWS/CodeBuild` and `Source/ThirdParty/GitHub`).
///
/// Source: the page `ActionTypeId$provider` links to, "Valid action types and providers in
/// CodePipeline" (reference-pipeline-structure.html#actions-valid-providers), and each
/// action's own structure reference — the same documentation the catalog table in
/// `state::built_in_catalog` is built from.
fn catalog_detail(region: &str) -> Vec<Row> {
    vec![
        Row {
            case: "an action type with no deep link and no required parameter",
            id: ("Approval", "AWS", "Manual", "1"),
            entry: json!({
                "id": {
                    "__type": "ActionTypeId",
                    "category": "Approval",
                    "owner": "AWS",
                    "provider": "Manual",
                    "version": "1",
                },
                "settings": {},
                "actionConfigurationProperties": [
                    described(
                        "NotificationArn",
                        "The Amazon SNS topic the approval request is published to",
                        false,
                        false,
                    ),
                    described(
                        "CustomData",
                        "The message displayed to the reviewer with the approval request",
                        false,
                        false,
                    ),
                    described(
                        "ExternalEntityLink",
                        "The URL of the information the reviewer needs to approve or reject",
                        false,
                        false,
                    ),
                ],
                "inputArtifactDetails": artifacts(0, 0),
                "outputArtifactDetails": artifacts(0, 0),
            }),
        },
        Row {
            case: "a source action type whose deep link names the Region and no parameter",
            id: ("Source", "AWS", "S3", "1"),
            entry: json!({
                "id": {
                    "__type": "ActionTypeId",
                    "category": "Source",
                    "owner": "AWS",
                    "provider": "S3",
                    "version": "1",
                },
                "settings": {
                    "entityUrlTemplate":
                        format!("https://console.aws.amazon.com/s3/home?region={region}#"),
                },
                "actionConfigurationProperties": [
                    described("S3Bucket", "The Amazon S3 Bucket", true, false),
                    described("S3ObjectKey", "The Amazon S3 object key", true, false),
                    described(
                        "PollForSourceChanges",
                        "The option to poll for source repository changes",
                        false,
                        false,
                    ),
                    described(
                        "AllowOverrideForS3ObjectKey",
                        "Whether S3 Object Key can be overridden during StartPipelineExecution",
                        false,
                        false,
                    ),
                ],
                "inputArtifactDetails": artifacts(0, 0),
                "outputArtifactDetails": artifacts(1, 1),
            }),
        },
        Row {
            case: "an invoke action type whose deep link names one of its parameters",
            id: ("Invoke", "AWS", "Lambda", "1"),
            entry: json!({
                "id": {
                    "__type": "ActionTypeId",
                    "category": "Invoke",
                    "owner": "AWS",
                    "provider": "Lambda",
                    "version": "1",
                },
                "settings": {
                    "entityUrlTemplate": format!(
                        "https://console.aws.amazon.com/lambda/home?region={region}\
                         #/functions/{{Config:FunctionName}}"
                    ),
                },
                "actionConfigurationProperties": [
                    described("FunctionName", "The AWS Lambda function to invoke", true, false),
                    described(
                        "UserParameters",
                        "Extra parameters for the AWS Lambda function",
                        false,
                        false,
                    ),
                ],
                "inputArtifactDetails": artifacts(0, 5),
                "outputArtifactDetails": artifacts(0, 5),
            }),
        },
        Row {
            case: "a build action type that publishes two deep links",
            id: ("Build", "AWS", "CodeBuild", "1"),
            entry: json!({
                "id": {
                    "__type": "ActionTypeId",
                    "category": "Build",
                    "owner": "AWS",
                    "provider": "CodeBuild",
                    "version": "1",
                },
                "settings": {
                    "entityUrlTemplate": format!(
                        "https://console.aws.amazon.com/codesuite/codebuild/projects/\
                         {{Config:ProjectName}}?region={region}"
                    ),
                    "executionUrlTemplate": format!(
                        "https://console.aws.amazon.com/codesuite/codebuild/projects/\
                         {{Config:ProjectName}}/history?region={region}"
                    ),
                },
                "actionConfigurationProperties": [
                    described(
                        "ProjectName",
                        "The CodeBuild build project that runs the action",
                        true,
                        false,
                    ),
                    described(
                        "PrimarySource",
                        "The input artifact whose source the build runs against",
                        false,
                        false,
                    ),
                    described(
                        "BatchEnabled",
                        "Whether the action runs the project as a batch build",
                        false,
                        false,
                    ),
                    described(
                        "CombineArtifacts",
                        "Whether a batch build combines its artifacts into one output artifact",
                        false,
                        false,
                    ),
                    described(
                        "EnvironmentVariables",
                        "The environment variables the build runs with, as a JSON array",
                        false,
                        false,
                    ),
                ],
                "inputArtifactDetails": artifacts(1, 5),
                "outputArtifactDetails": artifacts(0, 5),
            }),
        },
        Row {
            case: "a partner test action type with a secret credential and a sign-up page",
            id: ("Test", "ThirdParty", "GhostInspector", "1"),
            entry: json!({
                "id": {
                    "__type": "ActionTypeId",
                    "category": "Test",
                    "owner": "ThirdParty",
                    "provider": "GhostInspector",
                    "version": "1",
                },
                "settings": {
                    "entityUrlTemplate": "https://app.ghostinspector.com/tests/{Config:TestId}",
                    "executionUrlTemplate":
                        "https://app.ghostinspector.com/results/{ExternalExecutionId}",
                    "thirdPartyConfigurationUrl": "https://app.ghostinspector.com/code-pipeline",
                },
                "actionConfigurationProperties": [
                    described("ClientId", "The user client id", true, false),
                    described("ClientToken", "The user client token", true, true),
                    described(
                        "TestId",
                        "The test id used by the Ghost Inspector tests",
                        true,
                        false,
                    ),
                    described(
                        "ExtraParams",
                        "A JSON-encoded string of extra parameters to provide to the test or \
                         suite at runtime.",
                        false,
                        false,
                    ),
                ],
                "inputArtifactDetails": artifacts(0, 1),
                "outputArtifactDetails": artifacts(0, 0),
            }),
        },
        Row {
            case: "a partner source action type whose links name no Region",
            id: ("Source", "ThirdParty", "GitHub", "1"),
            entry: json!({
                "id": {
                    "__type": "ActionTypeId",
                    "category": "Source",
                    "owner": "ThirdParty",
                    "provider": "GitHub",
                    "version": "1",
                },
                "settings": {
                    "entityUrlTemplate":
                        "https://github.com/{Config:Owner}/{Config:Repo}/tree/{Config:Branch}",
                    "revisionUrlTemplate":
                        "https://github.com/{Config:Owner}/{Config:Repo}/commits",
                    "thirdPartyConfigurationUrl": "https://github.com/join",
                },
                "actionConfigurationProperties": [
                    described(
                        "Owner",
                        "The GitHub account or organization that owns the repository",
                        true,
                        false,
                    ),
                    described("Repo", "The repository source changes are detected in", true, false),
                    described("Branch", "The branch source changes are detected on", true, false),
                    described(
                        "OAuthToken",
                        "The token the pipeline reads the repository with",
                        true,
                        true,
                    ),
                    described(
                        "PollForSourceChanges",
                        "The option to poll for source repository changes",
                        false,
                        false,
                    ),
                ],
                "inputArtifactDetails": artifacts(0, 0),
                "outputArtifactDetails": artifacts(1, 1),
            }),
        },
    ]
}

/// RULE: each published action type reports its provider's documented detail — described
/// key properties in the documented order, the documented artifact bounds and the
/// provider's deep links.
#[tokio::test]
async fn each_published_action_type_reports_its_providers_documented_detail() {
    let emulator = Emulator::new();
    let region = "us-east-1";
    for Row { case, id, entry } in catalog_detail(region) {
        let reported = emulator.entry(region, &json!({}), id).await;
        assert_eq!(
            reported, entry,
            "case {case:?} ({id:?}): unexpected catalog entry, got {reported}"
        );
    }
}

/// RULE, over the Region the catalog is read for: a deep link into the console is a link
/// into ONE Region's console, so the Region a published template names is the Region the
/// listing was asked about — `regionFilter` when the request carries one, and otherwise
/// the Region the request itself was signed for. A partner's own pages name no Region, so
/// they read the same from everywhere.
#[tokio::test]
async fn a_published_deep_link_names_the_region_the_catalog_is_read_for() {
    let emulator = Emulator::new();

    // (case, the Region the call is signed for, regionFilter, the Region the links must
    // name). The Regions differ in partition prefix and in length, and the last row asks
    // about a Region other than the caller's own.
    let cases: [(&str, &str, Option<&str>, &str); 4] = [
        ("the signing Region", "us-east-1", None, "us-east-1"),
        ("another signing Region", "eu-central-1", None, "eu-central-1"),
        (
            "a filter naming the caller's own Region",
            "ap-southeast-2",
            Some("ap-southeast-2"),
            "ap-southeast-2",
        ),
        (
            "a filter naming another Region",
            "us-east-1",
            Some("us-west-2"),
            "us-west-2",
        ),
    ];

    for (case, signed_in, filter, expected) in cases {
        let body = match filter {
            None => json!({}),
            Some(filter) => json!({"regionFilter": filter}),
        };
        let listed = emulator.listed(signed_in, &body).await;
        assert!(
            !listed.is_empty(),
            "case {case:?}: the catalog reports its entries in every Region, got {listed:?}"
        );

        // The catalog's own table says which entries carry a Region at all, so this walks
        // every published template rather than a chosen few.
        for entry in &BuiltInCatalog::regional() {
            let reported = emulator.entry(signed_in, &body, entry.id).await;
            for member in &entry.members {
                let url = reported["settings"][member].as_str().unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: {:?} publishes {member}, got {reported}",
                        entry.id
                    )
                });
                assert!(
                    url.contains(expected),
                    "case {case:?}: {:?}'s {member} must name {expected}, got {url}",
                    entry.id
                );
                assert!(
                    !url.contains(CATALOG_REGION_PLACEHOLDER),
                    "case {case:?}: {:?}'s {member} must carry a Region rather than the \
                     catalog's placeholder, got {url}",
                    entry.id
                );
            }
        }
    }
}

/// The published entries whose deep links name a Region, with the `settings` members that
/// name it — read off the catalog table itself, so an entry added later is covered too.
struct BuiltInCatalog {
    id: (&'static str, &'static str, &'static str, &'static str),
    members: Vec<&'static str>,
}

impl BuiltInCatalog {
    fn regional() -> Vec<Self> {
        BUILT_IN_ACTION_TYPES
            .iter()
            .filter_map(|entry| {
                let members: Vec<&'static str> = [
                    ("entityUrlTemplate", entry.entity_url_template),
                    ("executionUrlTemplate", entry.execution_url_template),
                    ("revisionUrlTemplate", entry.revision_url_template),
                    (
                        "thirdPartyConfigurationUrl",
                        entry.third_party_configuration_url,
                    ),
                ]
                .into_iter()
                .filter(|(_, template)| {
                    template.is_some_and(|template| {
                        template.contains(CATALOG_REGION_PLACEHOLDER)
                    })
                })
                .map(|(member, _)| member)
                .collect();
                (!members.is_empty()).then_some(Self {
                    id: (entry.category, entry.owner, entry.provider, entry.version),
                    members,
                })
            })
            .collect()
    }
}

/// RULE as an INVARIANT over the WHOLE published catalog, so a provider added later cannot
/// break it: every entry reports described key properties and URL-shaped settings, only a
/// partner integration has a sign-up page, and no reported string carries the catalog's
/// own Region placeholder.
#[tokio::test]
async fn every_published_entry_is_described_and_carries_no_placeholder_region() {
    let emulator = Emulator::new();
    let region = "us-west-2";
    let listed = emulator.listed(region, &json!({})).await;
    assert_eq!(
        listed.len(),
        BUILT_IN_ACTION_TYPES.len(),
        "the listing reports every published action type, got {listed:?}"
    );

    for entry in &listed {
        let id = entry["id"].clone();
        let case = format!(
            "{}/{}/{}/{}",
            id["category"], id["owner"], id["provider"], id["version"]
        );

        for property in entry["actionConfigurationProperties"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: properties are a list, got {entry}"))
        {
            assert_eq!(
                property["key"].as_bool(),
                Some(true),
                "case {case}: every published property is a key, got {property}"
            );
            assert_eq!(
                property["queryable"].as_bool(),
                Some(false),
                "case {case}: no published property is queryable, got {property}"
            );
            let description = property["description"].as_str().unwrap_or_else(|| {
                panic!("case {case}: every published property is described, got {property}")
            });
            assert!(
                !description.trim().is_empty(),
                "case {case}: the description of {} must be text, got {description:?}",
                property["name"]
            );
        }

        let settings = entry["settings"]
            .as_object()
            .unwrap_or_else(|| panic!("case {case}: settings are a structure, got {entry}"));
        for (member, url) in settings {
            let url = url
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: {member} is a string, got {entry}"));
            assert!(
                url.starts_with("https://")
                    && (URL_LENGTH.0..=URL_LENGTH.1).contains(&url.chars().count()),
                "case {case}: {member} must be an https URL inside the modelled length \
                 {URL_LENGTH:?}, got {url}"
            );
            assert!(
                !url.contains(CATALOG_REGION_PLACEHOLDER),
                "case {case}: {member} must carry a Region rather than the catalog's \
                 placeholder, got {url}"
            );
        }
        let owner = id["owner"].as_str().unwrap_or_default();
        assert_eq!(
            settings.contains_key("thirdPartyConfigurationUrl"),
            owner == "ThirdParty",
            "case {case}: only a partner integration publishes a sign-up page, got {entry}"
        );
        if owner == "AWS" {
            assert!(
                entry["actionConfigurationProperties"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .all(|property| property["secret"] == json!(false)),
                "case {case}: an action type the service runs itself needs no secret of \
                 the caller's, got {entry}"
            );
        }
    }

    // The catalog spans both owners it can, and does carry a secret credential where the
    // provider needs one, so both flags are exercised in both directions.
    for owner in ["AWS", "ThirdParty"] {
        assert!(
            BUILT_IN_ACTION_TYPES
                .iter()
                .any(|entry: &BuiltInActionType| entry.owner == owner),
            "the catalog must publish at least one {owner}-owned action type"
        );
    }
    assert!(
        listed.iter().any(|entry| entry["actionConfigurationProperties"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|property| property["secret"] == json!(true))),
        "the catalog must publish a provider whose credential property is secret, got \
         {listed:?}"
    );
}
