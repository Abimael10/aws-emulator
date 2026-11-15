// SPDX-License-Identifier: CC0-1.0

//! Tag REMOVAL, and the two refusals the resource-tagging endpoints answer with.
//!
//! THE RULE: `UntagResource` removes exactly the tags whose keys the request names from the
//! resource its `ResourceArn` addresses, answers the modelled 204 with no body, and leaves
//! every other tag of that resource — and every other resource — untouched. A tag is
//! identified by its key alone, so naming a key the resource does not carry removes
//! nothing and is still a success, which is what makes the modelled idempotence hold.
//!
//! The two refusals belong to the tagging LAYER rather than to this service, so neither is
//! this service's `ResourceNotFoundException`: a `ResourceArn` that is not an ARN at all is
//! refused with `BadRequestException` and the fixed sentence `Invalid input resource arn:
//! bad syntax`, before any resource is looked up, and an ARN this service holds no resource
//! under with `NotFoundException` and the fixed sentence `Resource was not found`. Both
//! sentences are fixed text: neither echoes the value the caller sent.
//!
//! `ResourceArn` is a `@httpLabel` and `TagKeys` a `@required` `@httpQuery`, so a request
//! carrying neither addresses no modelled route at all — which is a routing failure rather
//! than one of these refusals, and is not what this suite is about.
//!
//! Every row drives the real wire path through [`super::harness`], and every removal is
//! read back through `ListTagsForResource` — the very map the removal lands on — so what is
//! asserted is what a later caller sees. The rows span what the rule generalises over: the
//! KIND of resource addressed (a connector, a template, a directory registration, which are
//! the three this service holds tags on), how many keys are named (none of them present,
//! one, several, all), a key the resource does not carry beside ones it does, and the second
//! call of a repeated removal. `TagKeys` is bound to the `tagKeys` query parameter
//! (`smithy_query(query="operation",operation="UntagResource")`), so each key travels as its
//! own occurrence of it.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{call, encode_label, register_directory, router, ACCOUNT, REGION};

/// The tags every resource in this suite is created with.
fn seeded_tags() -> Value {
    json!({ "owner": "platform", "tier": "gold", "env": "test" })
}

/// Which resource of this service a row addresses. All three hold tag maps; a service
/// principal name holds none, because it has no ARN of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Connector,
    Template,
    DirectoryRegistration,
}

const KINDS: [Kind; 3] = [Kind::Connector, Kind::Template, Kind::DirectoryRegistration];

/// Create one resource of `kind` carrying [`seeded_tags`] and answer its ARN. `suffix`
/// keeps the directory ids, client tokens and template names of two rows apart.
async fn given_tagged_resource(router: &MultiServiceRouter, kind: Kind, suffix: &str) -> String {
    match kind {
        Kind::DirectoryRegistration => given_registration(router, suffix).await,
        Kind::Connector => given_connector(router, suffix).await,
        Kind::Template => given_template(router, suffix).await,
    }
}

/// A directory registration carrying [`seeded_tags`].
async fn given_registration(router: &MultiServiceRouter, suffix: &str) -> String {
    let body = json!({
        "DirectoryId": directory_id_for(suffix),
        "ClientToken": format!("registration-{suffix}"),
        "Tags": seeded_tags(),
    });
    let (status, code, parsed) =
        call(router, "POST", "/directoryRegistrations", Some(&body)).await;
    assert_eq!(
        status, 202,
        "setup {suffix}: CreateDirectoryRegistration must answer 202, \
         got {status} / {code:?} / {parsed}"
    );
    string(&parsed, "DirectoryRegistrationArn", suffix)
}

/// A connector carrying [`seeded_tags`], under a directory registered for it (the
/// prerequisite `CreateConnector` resolves its `DirectoryId` against).
async fn given_connector(router: &MultiServiceRouter, suffix: &str) -> String {
    let directory_id = directory_id_for(suffix);
    register_directory(router, &directory_id).await;
    let body = json!({
        "DirectoryId": directory_id,
        "CertificateAuthorityArn": format!(
            "arn:aws:acm-pca:{REGION}:{ACCOUNT}:certificate-authority/\
             11111111-2222-3333-4444-555555555555"
        ),
        "VpcInformation": { "SecurityGroupIds": ["sg-0123456789abcdef0"] },
        "ClientToken": format!("connector-{suffix}"),
        "Tags": seeded_tags(),
    });
    let (status, code, parsed) = call(router, "POST", "/connectors", Some(&body)).await;
    assert_eq!(
        status, 202,
        "setup {suffix}: CreateConnector must answer 202, got {status} / {code:?} / {parsed}"
    );
    string(&parsed, "ConnectorArn", suffix)
}

/// A template carrying [`seeded_tags`], under a connector of its own.
async fn given_template(router: &MultiServiceRouter, suffix: &str) -> String {
    let connector = given_connector(router, suffix).await;
    let body = json!({
        "ConnectorArn": connector,
        "Name": format!("template-{suffix}"),
        "ClientToken": format!("template-{suffix}"),
        "Tags": seeded_tags(),
        "Definition": {
            "TemplateV2": {
                "CertificateValidity": {
                    "ValidityPeriod": { "PeriodType": "YEARS", "Period": 1 },
                    "RenewalPeriod": { "PeriodType": "WEEKS", "Period": 6 },
                },
                "PrivateKeyAttributes": {
                    "MinimalKeyLength": 2048,
                    "KeySpec": "KEY_EXCHANGE",
                },
                "PrivateKeyFlags": { "ClientVersion": "WINDOWS_SERVER_2016" },
                "EnrollmentFlags": { "NoSecurityExtension": true },
                "SubjectNameFlags": { "SanRequireDirectoryGuid": true },
                "GeneralFlags": { "MachineType": true },
                "Extensions": {
                    "KeyUsage": { "UsageFlags": { "KeyEncipherment": true } }
                },
            }
        },
    });
    let (status, code, parsed) = call(router, "POST", "/templates", Some(&body)).await;
    assert_eq!(
        status, 200,
        "setup {suffix}: CreateTemplate must answer 200, got {status} / {code:?} / {parsed}"
    );
    string(&parsed, "TemplateArn", suffix)
}

/// A directory id of the modelled form, derived from `suffix` so two rows never register
/// the same directory (`DirectoryId` -> `^d-[0-9a-f]{10}$`).
fn directory_id_for(suffix: &str) -> String {
    let digits: String = suffix
        .bytes()
        .map(|byte| char::from(b'0' + (byte % 10)))
        .take(10)
        .collect();
    format!("d-{:0<10}", digits)
}

/// One required string member of a response body.
fn string(parsed: &Value, member: &str, case: &str) -> String {
    parsed[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} in {parsed}"))
        .to_string()
}

/// `ListTagsForResource` on `arn`: the status, the error code and the tags it reports.
async fn list_tags(
    router: &MultiServiceRouter,
    arn: &str,
) -> (u16, String, BTreeMap<String, String>, Value) {
    let (status, code, parsed) = call(
        router,
        "GET",
        &format!("/tags/{}", encode_label(arn)),
        None,
    )
    .await;
    let tags = parsed["Tags"]
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value.as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    (status, code, tags, parsed)
}

/// `UntagResource` on `arn` for `keys`, each key its own `tagKeys` query occurrence.
async fn untag(
    router: &MultiServiceRouter,
    arn: &str,
    keys: &[&str],
) -> (u16, String, Value) {
    let query: Vec<String> = keys
        .iter()
        .map(|key| format!("tagKeys={}", encode_label(key)))
        .collect();
    let target = if query.is_empty() {
        format!("/tags/{}", encode_label(arn))
    } else {
        format!("/tags/{}?{}", encode_label(arn), query.join("&"))
    };
    call(router, "DELETE", &target, None).await
}

/// One removal: the keys it names and what the resource is left carrying.
struct RemovalCase {
    /// Why this row is here.
    case: &'static str,
    /// The keys the request names.
    keys: &'static [&'static str],
    /// The tags the resource carries afterwards, as pairs.
    remaining: &'static [(&'static str, &'static str)],
}

/// The rows, each applied to every kind of resource that holds tags.
const REMOVALS: &[RemovalCase] = &[
    RemovalCase {
        case: "one of the keys the resource carries",
        keys: &["tier"],
        remaining: &[("owner", "platform"), ("env", "test")],
    },
    RemovalCase {
        case: "several of the keys it carries",
        keys: &["tier", "env"],
        remaining: &[("owner", "platform")],
    },
    RemovalCase {
        case: "every key it carries",
        keys: &["owner", "tier", "env"],
        remaining: &[],
    },
    RemovalCase {
        case: "a key it does not carry, beside one it does",
        keys: &["absent", "owner"],
        remaining: &[("tier", "gold"), ("env", "test")],
    },
    RemovalCase {
        case: "only keys it does not carry",
        keys: &["absent", "also-absent"],
        remaining: &[("owner", "platform"), ("tier", "gold"), ("env", "test")],
    },
];

/// The expected map of a row's `remaining`.
fn expected(remaining: &[(&str, &str)]) -> BTreeMap<String, String> {
    remaining
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

#[tokio::test]
async fn untag_resource_drops_exactly_the_named_keys_and_answers_the_modelled_204() {
    let router = router();
    for (index, row) in REMOVALS.iter().enumerate() {
        for kind in KINDS {
            let case = format!("{} on a {kind:?}", row.case);
            let suffix = format!("{index}{}", KINDS.iter().position(|k| *k == kind).unwrap());
            let arn = given_tagged_resource(&router, kind, &suffix).await;

            let (status, code, parsed) = untag(&router, &arn, row.keys).await;
            assert_eq!(
                (status, code.as_str()),
                (204, ""),
                "case {case:?}: a removal answers the modelled 204 with no error, \
                 got {status} / {code:?} / {parsed}"
            );
            assert_eq!(
                parsed,
                Value::Null,
                "case {case:?}: a 204 carries no body, got {parsed}"
            );

            // Read back through the operation that reports the very map the removal
            // landed on.
            let (status, code, tags, parsed) = list_tags(&router, &arn).await;
            assert_eq!(
                (status, code.as_str()),
                (200, ""),
                "case {case:?}: the tags are read back with 200, \
                 got {status} / {code:?} / {parsed}"
            );
            assert_eq!(
                tags,
                expected(row.remaining),
                "case {case:?}: expected {:?} to be left, got {parsed}",
                expected(row.remaining)
            );

            // Idempotent: the same removal again changes nothing and is still a 204.
            let (status, _, _) = untag(&router, &arn, row.keys).await;
            let (_, _, repeated, parsed) = list_tags(&router, &arn).await;
            assert_eq!(
                (status, repeated),
                (204, expected(row.remaining)),
                "case {case:?}: repeating the removal changes nothing, got {parsed}"
            );
        }
    }
}

/// A removal reaches ONE resource: every other resource of this service keeps its tags,
/// including another resource of the same kind and the connector a template hangs off.
#[tokio::test]
async fn untagging_one_resource_leaves_every_other_resource_alone() {
    let router = router();
    let first = given_tagged_resource(&router, Kind::Connector, "aa").await;
    let second = given_tagged_resource(&router, Kind::Connector, "bb").await;
    let registration = given_tagged_resource(&router, Kind::DirectoryRegistration, "cc").await;

    let (status, code, parsed) = untag(&router, &first, &["owner", "tier", "env"]).await;
    assert_eq!(
        (status, code.as_str()),
        (204, ""),
        "case \"the addressed connector\": expected 204, got {status} / {code:?} / {parsed}"
    );

    for (case, arn) in [
        ("another connector", &second),
        ("a directory registration", &registration),
    ] {
        let (_, _, tags, parsed) = list_tags(&router, arn).await;
        assert_eq!(
            tags,
            expected(&[("owner", "platform"), ("tier", "gold"), ("env", "test")]),
            "case {case:?}: a removal on another resource must change nothing here, \
             got {parsed}"
        );
    }
}

/// The tagging layer's two refusals, over both endpoints that address a resource by ARN:
/// a value it cannot parse as an ARN, and an ARN it resolves to no resource of this
/// service. Both sentences are fixed text, so neither echoes the value sent.
#[tokio::test]
async fn the_tagging_layer_refuses_a_malformed_arn_and_an_arn_naming_nothing() {
    let router = router();
    // One resource exists, so "nothing is found" is a fact about the ARN and not about an
    // empty store.
    let held = given_tagged_resource(&router, Kind::Connector, "dd").await;

    // (case, the ResourceArn, the expected error code, the expected sentence)
    let rows: Vec<(&str, String, &str, &str)> = vec![
        (
            "a word",
            "definitely-not-an-arn".to_string(),
            "BadRequestException",
            "Invalid input resource arn: bad syntax",
        ),
        (
            "an ARN missing its resource field",
            format!("arn:aws:pca-connector-ad:{REGION}:{ACCOUNT}:"),
            "BadRequestException",
            "Invalid input resource arn: bad syntax",
        ),
        (
            "a well-formed connector ARN this service never minted",
            format!(
                "arn:aws:pca-connector-ad:{REGION}:{ACCOUNT}:connector/\
                 99999999-8888-7777-6666-555555555555"
            ),
            "NotFoundException",
            "Resource was not found",
        ),
        (
            "a well-formed template ARN this service never minted",
            format!("{held}/template/99999999-8888-7777-6666-555555555555"),
            "NotFoundException",
            "Resource was not found",
        ),
        (
            "another vendor's ARN, which parses and names nothing here",
            "arn:aws:s3:::not-a-pca-connector-ad-resource".to_string(),
            "NotFoundException",
            "Resource was not found",
        ),
    ];

    for (case, resource_arn, expected_code, sentence) in &rows {
        let (status, code, parsed) = untag(&router, resource_arn, &["owner"]).await;
        assert_eq!(
            (status, code.as_str(), parsed["message"].as_str()),
            (400, *expected_code, Some(*sentence)),
            "case {case:?} on UntagResource: expected 400 / {expected_code} / {sentence:?}, \
             got {status} / {code:?} / {parsed}"
        );

        let (status, code, _, parsed) = list_tags(&router, resource_arn).await;
        assert_eq!(
            (status, code.as_str(), parsed["message"].as_str()),
            (400, *expected_code, Some(*sentence)),
            "case {case:?} on ListTagsForResource: expected 400 / {expected_code} / \
             {sentence:?}, got {status} / {code:?} / {parsed}"
        );
    }

    // A refused removal removed nothing: the one resource that does exist still carries
    // every tag it was created with.
    let (_, _, tags, parsed) = list_tags(&router, &held).await;
    assert_eq!(
        tags,
        expected(&[("owner", "platform"), ("tier", "gold"), ("env", "test")]),
        "case \"the held connector after the refusals\": expected its tags untouched, \
         got {parsed}"
    );
}
