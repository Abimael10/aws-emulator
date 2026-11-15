// SPDX-License-Identifier: CC0-1.0

//! What a `{{resolve:secretsmanager:<secret id>:SecretString:<json key>}}` dynamic reference
//! fails its resource with when the secret associates NO value with that key, end to end
//! over the awsQuery wire.
//!
//! RULE: a `secretsmanager` dynamic reference naming a `json-key` the referenced secret's
//! `SecretString` associates no value with fails the resource that carries it with ONE FIXED
//! reason — `Could not find a value associated with JSONKey in SecretString` — whatever the
//! reference names, however it names it, and whatever resource type and property carry it:
//! nothing of the request (the secret's name or ARN, the key, the logical id, the property)
//! is written into that reason, so a client classifying a deploy failure by its reason
//! compares against a constant rather than an environment-dependent sentence. The rest of
//! the failure is the ordinary one for an unresolvable reference: the carrier is the ONE
//! resource that fails, the stack rolls back to `ROLLBACK_COMPLETE`, and the resource the
//! declaration would have created does not exist afterwards.
//!
//! The rows differ along what the rule generalises over: WHY no value is associated with the
//! key (a JSON document holding other keys, a key differing from a held one only in case, a
//! `SecretString` that is not a JSON document at all, one that is a JSON array and so
//! associates no key with anything), HOW the reference names the secret (by ARN, by name,
//! and with a version stage stated beside the key), and WHICH resource type and property
//! carry the reference (`AWS::SecretsManager::Secret`'s `SecretString` and
//! `AWS::SSM::Parameter`'s `Value`).
//!
//! The boundary of the rule is the last case: the SAME references naming keys the secret
//! does hold, and one naming no key at all, resolve to the held values and deploy.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::{
    resource_status, status, STACK_RESOURCE_TYPE,
};

use crate::dynamic_references::ssm;
use crate::stack_lifecycle::{
    call, create_stack, member, router, settled_stack, xml_blocks, xml_text,
};
use crate::stack_owned_secret_teardown::{error_code, secretsmanager};
use crate::stack_resources::element;

/// The WHOLE `ResourceStatusReason` CloudFormation states for a `secretsmanager` reference
/// whose `json-key` the secret associates no value with: recorded behaviour, and fixed text —
/// it carries nothing of the reference, the secret or the property.
const NO_VALUE_FOR_THE_JSON_KEY: &str =
    "Could not find a value associated with JSONKey in SecretString";

/// The error code Secrets Manager refuses a read of a secret it does not hold with
/// (`ResourceNotFoundException`, that service's own modelled refusal).
const SECRET_NOT_FOUND: &str = "ResourceNotFoundException";

/// The error code Parameter Store refuses a read of a name it holds no parameter under with
/// (`ParameterNotFound`, Systems Manager's own modelled refusal of `GetParameter`).
const PARAMETER_NOT_FOUND: &str = "ParameterNotFound";

/// The logical id every row declares its carrier under.
const CARRIER: &str = "Copy";

/// One source secret a row's reference resolves against: created through Secrets Manager's
/// OWN wire, so the reference resolves against the very secret `CreateSecret` minted.
#[derive(Debug, Clone, Copy)]
struct Source {
    name: &'static str,
    secret_string: &'static str,
}

/// A JSON document holding two keys — the recorded shape of a credential secret.
const DOCUMENT: Source = Source {
    name: "dynref-missing-key-document",
    secret_string: r#"{"username":"app","password":"pw-value"}"#,
};

/// A `SecretString` that is not a JSON document at all, so no key of it can be addressed.
const PLAIN_TEXT: Source = Source {
    name: "dynref-missing-key-plain-text",
    secret_string: "a-plain-text-example-value",
};

/// A `SecretString` that IS JSON but associates no key with a value.
const ARRAY: Source = Source {
    name: "dynref-missing-key-array",
    secret_string: r#"["app","pw-value"]"#,
};

/// Every source a row resolves against, created once per test: each row addresses one of
/// them, and several rows address the same one.
const SOURCES: &[Source] = &[DOCUMENT, PLAIN_TEXT, ARRAY];

/// Which resource type and property a row stands the reference in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Carrier {
    /// `AWS::SecretsManager::Secret`'s `SecretString` — the recorded carrier: a secret whose
    /// value is copied out of another secret.
    SecretString,
    /// `AWS::SSM::Parameter`'s `Value` — a type of another service entirely, and a property
    /// whose deployed value that service reports back.
    ParameterValue,
}

impl Carrier {
    /// The declaration of one carrier: `name` is the resource the deployment would create and
    /// `value` the property the reference stands in.
    fn declaration(self, name: &str, value: &str) -> Value {
        match self {
            Self::SecretString => json!({
                "Type": "AWS::SecretsManager::Secret",
                "Properties": {"Name": name, "SecretString": value}
            }),
            Self::ParameterValue => json!({
                "Type": "AWS::SSM::Parameter",
                "Properties": {"Name": name, "Type": "String", "Value": value}
            }),
        }
    }

    /// Assert the resource the declaration would have created is not there at all, refused
    /// by the service that would own it.
    async fn assert_absent(self, router: &MultiServiceRouter, case: &str, name: &str) {
        let (status_code, refused, expected) = match self {
            Self::SecretString => {
                let (status_code, refused) =
                    secretsmanager(router, "DescribeSecret", &json!({"SecretId": name})).await;
                (status_code, refused, SECRET_NOT_FOUND)
            }
            Self::ParameterValue => {
                let (status_code, refused) = ssm(
                    router,
                    "GetParameter",
                    &json!({"Name": name, "WithDecryption": true}),
                )
                .await;
                (status_code, refused, PARAMETER_NOT_FOUND)
            }
        };
        assert_eq!(
            (status_code, error_code(case, &refused).as_str()),
            (400, expected),
            "case {case:?}: the rolled-back resource {name} must not exist, so its read must \
             be refused with {expected}, got {status_code} {refused}"
        );
    }

    /// What the deployed resource carries for the property the reference stood in, where its
    /// owning service reports it back.
    async fn deployed_value(
        self,
        router: &MultiServiceRouter,
        case: &str,
        name: &str,
    ) -> Option<String> {
        match self {
            Self::SecretString => {
                let (status_code, answered) =
                    secretsmanager(router, "GetSecretValue", &json!({"SecretId": name})).await;
                assert_eq!(
                    status_code, 200,
                    "case {case:?}: GetSecretValue({name}) must answer 200, got {status_code} \
                     {answered}"
                );
                answered
                    .get("SecretString")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }
            Self::ParameterValue => {
                let (status_code, answered) = ssm(
                    router,
                    "GetParameter",
                    &json!({"Name": name, "WithDecryption": true}),
                )
                .await;
                assert_eq!(
                    status_code, 200,
                    "case {case:?}: GetParameter({name}) must answer 200, got {status_code} \
                     {answered}"
                );
                answered
                    .pointer("/Parameter/Value")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }
        }
    }
}

/// How a row's reference addresses the source secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddressedBy {
    /// The ARN Secrets Manager minted for it, which carries colons of its own.
    Arn,
    /// The secret's NAME.
    Name,
}

/// One row: a reference that resolves to no value, and the deployment it fails.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// The stack the row deploys, which also names the resource the carrier would create so
    /// no two rows can collide.
    stack: &'static str,
    source: Source,
    addressed_by: AddressedBy,
    /// The `json-key` the reference names, which the source associates no value with.
    json_key: &'static str,
    /// The version STAGE the reference states beside the key, or `""` for a reference that
    /// states none.
    version_stage: &'static str,
    carrier: Carrier,
}

const ROWS: &[Row] = &[
    Row {
        // The recorded case: a document holding other keys, addressed by its ARN.
        case: "a JSON document that holds other keys, addressed by ARN",
        stack: "dynref-missing-key-document-arn",
        source: DOCUMENT,
        addressed_by: AddressedBy::Arn,
        json_key: "no_such_key",
        version_stage: "",
        carrier: Carrier::SecretString,
    },
    Row {
        // A key is matched exactly: one differing from a held key only in case is absent.
        case: "a key differing from a held one only in case, in another service's property",
        stack: "dynref-missing-key-case",
        source: DOCUMENT,
        addressed_by: AddressedBy::Arn,
        json_key: "Password",
        version_stage: "",
        carrier: Carrier::ParameterValue,
    },
    Row {
        // The secret holds text, but no document for a key to be looked up in.
        case: "a SecretString that is not a JSON document, addressed by name",
        stack: "dynref-missing-key-plain-text",
        source: PLAIN_TEXT,
        addressed_by: AddressedBy::Name,
        json_key: "password",
        version_stage: "",
        carrier: Carrier::SecretString,
    },
    Row {
        // JSON, but a form that associates no key with anything.
        case: "a SecretString that is a JSON array",
        stack: "dynref-missing-key-array",
        source: ARRAY,
        addressed_by: AddressedBy::Arn,
        json_key: "password",
        version_stage: "",
        carrier: Carrier::ParameterValue,
    },
    Row {
        // The reference selects a version as well, which changes nothing about the reason.
        case: "a missing key on a reference that states a version stage too",
        stack: "dynref-missing-key-staged",
        source: DOCUMENT,
        addressed_by: AddressedBy::Name,
        json_key: "no_such_key",
        version_stage: "AWSCURRENT",
        carrier: Carrier::SecretString,
    },
];

/// The resource a row's carrier would have created, named after its stack so the rows cannot
/// collide in either owning service.
fn carried_resource(row: &Row) -> String {
    format!("{}-copy", row.stack)
}

/// The `ClientRequestToken` one create carries: Secrets Manager's `ClientRequestTokenType`
/// is `@length` 32..64 and names the version the write creates, so the source's own name is
/// padded out to that minimum — one token per source, and no two alike.
fn version_token(source: Source) -> String {
    format!("{:0<32}", source.name)
}

/// Create every source secret through Secrets Manager's OWN wire, and answer the ARN that
/// service minted for each, by the name it was created under — so a reference resolves
/// against the very secret `CreateSecret` minted.
async fn create_sources(router: &MultiServiceRouter) -> BTreeMap<&'static str, String> {
    let mut minted = BTreeMap::new();
    for source in SOURCES {
        let (status_code, answered) = secretsmanager(
            router,
            "CreateSecret",
            &json!({
                "Name": source.name,
                "SecretString": source.secret_string,
                "ClientRequestToken": version_token(*source)
            }),
        )
        .await;
        assert_eq!(
            status_code, 200,
            "CreateSecret({}) must answer 200, got {status_code} {answered}",
            source.name
        );
        let arn = answered
            .get("ARN")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("CreateSecret({}) published no ARN: {answered}", source.name)
            })
            .to_string();
        minted.insert(source.name, arn);
    }
    minted
}

/// The reference one row's carrier declares, against the secret id it addresses.
fn reference(row: &Row, secret_id: &str) -> String {
    let mut fields = format!("{secret_id}:SecretString:{}", row.json_key);
    if !row.version_stage.is_empty() {
        fields.push(':');
        fields.push_str(row.version_stage);
    }
    format!("{{{{resolve:secretsmanager:{fields}}}}}")
}

/// Every `(LogicalResourceId, ResourceStatus, ResourceStatusReason)` the stack recorded for a
/// resource of its own template that FAILED — the stack's own events (whose resource type is
/// the stack type) are the operation's summary, not a resource that failed.
async fn resource_failures(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, String)> {
    let (status_code, body) =
        call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: DescribeStackEvents({stack}) must answer 200, got {status_code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|entry| element(entry, "ResourceType").as_deref() != Some(STACK_RESOURCE_TYPE))
        .filter_map(|entry| {
            let reported = element(entry, "ResourceStatus")?;
            if !reported.ends_with("_FAILED") {
                return None;
            }
            Some((
                element(entry, "LogicalResourceId").unwrap_or_default(),
                reported,
                element(entry, "ResourceStatusReason").unwrap_or_default(),
            ))
        })
        .collect()
}

#[tokio::test]
async fn a_reference_to_a_json_key_the_secret_does_not_associate_states_the_fixed_reason() {
    let router = router();
    let minted = create_sources(&router).await;
    for row in ROWS {
        let case = row.case;
        let arn = minted[row.source.name].clone();
        let secret_id = match row.addressed_by {
            AddressedBy::Arn => arn.clone(),
            AddressedBy::Name => row.source.name.to_string(),
        };
        let created = carried_resource(row);
        let template = json!({
            "Resources": {
                CARRIER: row
                    .carrier
                    .declaration(&created, &reference(row, &secret_id))
            }
        })
        .to_string();

        let (status_code, body) = create_stack(&router, row.stack, &template, &[]).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: CreateStack must answer 200, got {status_code} {body}"
        );
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::ROLLBACK_COMPLETE),
            "case {case:?}: expected the unresolvable reference to roll the stack back, got \
             {described}"
        );

        let failures = resource_failures(&router, case, row.stack).await;
        assert_eq!(
            failures,
            vec![(
                CARRIER.to_string(),
                resource_status::CREATE_FAILED.to_string(),
                NO_VALUE_FOR_THE_JSON_KEY.to_string(),
            )],
            "case {case:?}: expected the resource carrying the reference to be the one \
             failure, stating {NO_VALUE_FOR_THE_JSON_KEY:?}, got {failures:?}"
        );
        // The reason is a CONSTANT: none of the per-run values the reference was written
        // with is interpolated into it, so it compares equal in any account and Region.
        let reason = &failures[0].2;
        for interpolated in [
            arn.as_str(),
            row.source.name,
            row.json_key,
            created.as_str(),
            CARRIER,
        ] {
            assert!(
                !reason.contains(interpolated),
                "case {case:?}: the reason must carry nothing of the request, but \
                 {reason:?} carries {interpolated:?}"
            );
        }

        row.carrier.assert_absent(&router, case, &created).await;
    }
}

/// One resource of the boundary case: the same reference form, naming a value the secret DOES
/// associate, resolves to it.
#[derive(Debug, Clone, Copy)]
struct Honoured {
    case: &'static str,
    logical_id: &'static str,
    source: Source,
    /// The `json-key` the reference names, or `""` for a reference naming NO key, which
    /// resolves to the whole `SecretString`.
    json_key: &'static str,
    carrier: Carrier,
    /// The value the deployed resource must carry.
    expected: &'static str,
}

const HONOURED: &[Honoured] = &[
    Honoured {
        case: "a key the document holds, copied into another secret",
        logical_id: "Password",
        source: DOCUMENT,
        json_key: "password",
        carrier: Carrier::SecretString,
        expected: "pw-value",
    },
    Honoured {
        case: "another key of the same document, in another service's property",
        logical_id: "Username",
        source: DOCUMENT,
        json_key: "username",
        carrier: Carrier::ParameterValue,
        expected: "app",
    },
    Honoured {
        case: "a reference naming no key at all, which resolves to the whole SecretString",
        logical_id: "Whole",
        source: PLAIN_TEXT,
        json_key: "",
        carrier: Carrier::ParameterValue,
        expected: "a-plain-text-example-value",
    },
];

/// The stack the boundary case deploys.
const HONOURED_STACK: &str = "dynref-held-key";

/// The resource one honoured row creates.
fn honoured_resource(row: &Honoured) -> String {
    format!("{HONOURED_STACK}-{}", row.logical_id.to_lowercase())
}

#[tokio::test]
async fn a_reference_to_a_value_the_secret_does_associate_is_honoured() {
    let router = router();
    create_sources(&router).await;
    let mut resources = serde_json::Map::new();
    for row in HONOURED {
        let reference = if row.json_key.is_empty() {
            format!("{{{{resolve:secretsmanager:{}}}}}", row.source.name)
        } else {
            format!(
                "{{{{resolve:secretsmanager:{}:SecretString:{}}}}}",
                row.source.name, row.json_key
            )
        };
        resources.insert(
            row.logical_id.to_string(),
            row.carrier
                .declaration(&honoured_resource(row), &reference),
        );
    }
    let template = json!({"Resources": resources}).to_string();

    let (status_code, body) = create_stack(&router, HONOURED_STACK, &template, &[]).await;
    assert_eq!(
        status_code, 200,
        "CreateStack must answer 200, got {status_code} {body}"
    );
    let described = settled_stack(&router, "the honoured references", HONOURED_STACK).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "expected a reference the store can honour to deploy, got {described}"
    );

    for row in HONOURED {
        let case = row.case;
        let created = honoured_resource(row);
        let deployed = row.carrier.deployed_value(&router, case, &created).await;
        assert_eq!(
            deployed.as_deref(),
            Some(row.expected),
            "case {case:?}: expected the deployed resource to carry the value the secret \
             associates ({:?}), got {deployed:?}",
            row.expected
        );
    }
}
