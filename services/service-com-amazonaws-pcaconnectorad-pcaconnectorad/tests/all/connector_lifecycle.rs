// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the pca-connector-ad connector/template slice, down to
//! the group access control entries a template carries.
//!
//! Every case drives the real wire path: a `restJson1` request through the same
//! `MultiServiceRouter::dispatch` the production front door uses, so routing,
//! `@httpLabel` binding, constraint validation, the handler and serialization are
//! all exercised. Nothing reaches into the state struct. The request path itself
//! — the router, the request builder, the `@httpLabel` encoding and the error-code
//! read — comes from [`super::harness`], which every suite in this crate shares.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{
    call as dispatch, call_at_host, encode_label, register_directory, router, ACCOUNT,
    ACCOUNT_KEY_ID, REGION,
};

// The store bounds the handlers refuse past, imported from the crate so these
// tests cannot drift from the caps the handlers enforce.
use service_com_amazonaws_pcaconnectorad_pcaconnectorad::state::{
    CONNECTOR_ACTIVATION_DELAY, MAX_ACCESS_CONTROL_ENTRIES_PER_TEMPLATE, MAX_CONNECTORS,
    MAX_TEMPLATES_PER_CONNECTOR,
};

/// The Microsoft AD certificate-template OID arc a template's `ObjectIdentifier`
/// sits under (see `state::template_object_identifier`).
const AD_TEMPLATE_OID_ARC: &str = "1.3.6.1.4.1.311.21.8";

/// Past the handler's activation delay by a wide margin, so a connector read
/// after it has certainly settled. Derived from the crate's own constant, so the
/// case cannot drift from the span the handler stores.
const PAST_ACTIVATION: std::time::Duration =
    CONNECTOR_ACTIVATION_DELAY.saturating_add(std::time::Duration::from_millis(750));

/// Status plus parsed JSON body of a dispatched request, with the wire error code
/// surfaced as `__code` so a case can assert it without caring whether the
/// protocol put it in the header or the body.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    path_and_query: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let (status, code, parsed) = dispatch(router, method, path_and_query, body).await;
    let parsed = match (code.is_empty(), parsed) {
        (true, parsed) => parsed,
        (false, Value::Object(mut map)) => {
            map.insert("__code".to_string(), Value::String(code));
            Value::Object(map)
        }
        (false, _) => json!({ "__code": code }),
    };
    (status, parsed)
}

/// Whether `value` is a lowercase hyphenated random (version 4) UUID, the form
/// the connector and template ARN patterns require: 8-4-4-4-12 hex groups, the
/// version nibble `4` leading the third group and an RFC 4122 §4.4 variant
/// nibble (`8`, `9`, `a` or `b`) leading the fourth.
fn is_uuid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    let shaped = groups.len() == 5
        && [8usize, 4, 4, 4, 12]
            .iter()
            .zip(&groups)
            .all(|(len, group)| {
                group.len() == *len
                    && group
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            });
    shaped
        && groups[2].starts_with('4')
        && groups[3]
            .chars()
            .next()
            .is_some_and(|variant| matches!(variant, '8' | '9' | 'a' | 'b'))
}

fn connector_arn_prefix() -> String {
    format!("arn:aws:pca-connector-ad:{REGION}:{ACCOUNT}:connector/")
}

/// One row of the connector table: inputs that differ along what the rule
/// generalises over (directory id, CA, security-group count, IP address type,
/// tag count).
struct ConnectorCase {
    name: &'static str,
    directory_id: &'static str,
    certificate_authority_arn: &'static str,
    security_group_ids: &'static [&'static str],
    ip_address_type: Option<&'static str>,
    tags: &'static [(&'static str, &'static str)],
}

/// `IpAddressType` values come from the modelled enum (`IP_ADDRESS_TYPE_VALUES`:
/// `IPV4`, `DUALSTACK`); `SecurityGroupIds` is modelled `length 1..4`, so the
/// rows span both published bounds.
const CONNECTOR_CASES: &[ConnectorCase] = &[
    ConnectorCase {
        name: "one security group, no ip address type, no tags",
        directory_id: "d-0123456789",
        certificate_authority_arn:
            "arn:aws:acm-pca:us-west-2:000000000000:certificate-authority/11111111-2222-3333-4444-555555555555",
        security_group_ids: &["sg-0123456789abcdef0"],
        ip_address_type: None,
        tags: &[],
    },
    ConnectorCase {
        name: "four security groups at the modelled maximum, IPV4, one tag",
        directory_id: "d-abcdef0123",
        certificate_authority_arn:
            "arn:aws:acm-pca:us-west-2:000000000000:certificate-authority/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        security_group_ids: &[
            "sg-00000000000000001",
            "sg-00000000000000002",
            "sg-00000000000000003",
            "sg-00000000000000004",
        ],
        ip_address_type: Some("IPV4"),
        tags: &[("Team", "pki")],
    },
    ConnectorCase {
        name: "two security groups, DUALSTACK, several tags",
        directory_id: "d-00000000ff",
        certificate_authority_arn:
            "arn:aws:acm-pca:us-west-2:000000000000:certificate-authority/0f0f0f0f-1e1e-2d2d-3c3c-4b4b4b4b4b4b",
        security_group_ids: &["sg-000000000000000ab", "sg-000000000000000cd"],
        ip_address_type: Some("DUALSTACK"),
        tags: &[("Env", "test"), ("Owner", "ad")],
    },
];

impl ConnectorCase {
    fn body(&self) -> Value {
        let mut vpc = json!({ "SecurityGroupIds": self.security_group_ids });
        if let Some(ip_address_type) = self.ip_address_type {
            vpc["IpAddressType"] = json!(ip_address_type);
        }
        let mut body = json!({
            "DirectoryId": self.directory_id,
            "CertificateAuthorityArn": self.certificate_authority_arn,
            "VpcInformation": vpc,
        });
        if !self.tags.is_empty() {
            body["Tags"] = Value::Object(
                self.tags
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), json!(v)))
                    .collect(),
            );
        }
        body
    }
}

/// `CreateConnector` for `body`, answering the ARN it minted.
///
/// `pub(crate)` because the sibling suites that need a connector to hang a child
/// resource off need the same prerequisite chain, and a second copy of it is how two
/// suites come to set up two different things.
pub(crate) async fn create_connector(router: &MultiServiceRouter, body: &Value) -> String {
    // A connector's directory must already be registered with this service: the
    // create resolves its `DirectoryId` against the directory-registration store,
    // so the prerequisite is established before every case below.
    register_directory(
        router,
        body["DirectoryId"]
            .as_str()
            .unwrap_or_else(|| panic!("a connector body must carry a DirectoryId: {body}")),
    )
    .await;
    let (status, parsed) = call(router, "POST", "/connectors", Some(body)).await;
    assert_eq!(
        status, 202,
        "CreateConnector must answer the modelled 202, got {status} / {parsed}"
    );
    parsed["ConnectorArn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateConnector returned no ConnectorArn: {parsed}"))
        .to_string()
}

/// The `Arn` of every summary in a `ListConnectors` response, in the order the
/// service returned them.
fn listed_arns(listed: &Value) -> Vec<String> {
    listed["Connectors"]
        .as_array()
        .map(|summaries| {
            summaries
                .iter()
                .filter_map(|summary| summary["Arn"].as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

async fn get_connector(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/connectors/{}", encode_label(arn)),
        None,
    )
    .await
}

#[tokio::test]
async fn create_connector_mints_a_modelled_arn_and_get_connector_reads_the_record_back() {
    let router = router();

    for case in CONNECTOR_CASES {
        let body = case.body();
        let started = std::time::Instant::now();
        let arn = create_connector(&router, &body).await;

        let prefix = connector_arn_prefix();
        assert!(
            arn.starts_with(&prefix),
            "case {}: expected a connector ARN starting {prefix}, got {arn:?}",
            case.name
        );
        assert!(
            is_uuid(&arn[prefix.len()..]),
            "case {}: expected a uuid after `connector/`, got {arn:?}",
            case.name
        );

        let (status, parsed) = get_connector(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {}: expected GetConnector 200, got {status} / {parsed}",
            case.name
        );
        let connector = &parsed["Connector"];
        assert_eq!(
            connector["Arn"].as_str(),
            Some(arn.as_str()),
            "case {}: GetConnector must report the ARN it was asked for, got {connector}",
            case.name
        );
        assert_eq!(
            connector["DirectoryId"], body["DirectoryId"],
            "case {}: DirectoryId must be what CreateConnector persisted, got {connector}",
            case.name
        );
        assert_eq!(
            connector["CertificateAuthorityArn"], body["CertificateAuthorityArn"],
            "case {}: CertificateAuthorityArn must round-trip, got {connector}",
            case.name
        );
        assert_eq!(
            connector["VpcInformation"], body["VpcInformation"],
            "case {}: VpcInformation must round-trip, got {connector}",
            case.name
        );
        // The connector is CREATING until the clock-owed activation delay has
        // passed; the read below is well inside it unless the host is so loaded
        // that the whole round trip outran the delay, in which case the settled
        // value is the correct one. The dedicated settle case proves the move.
        let elapsed = started.elapsed();
        let expected_status = if elapsed < CONNECTOR_ACTIVATION_DELAY {
            "CREATING"
        } else {
            "ACTIVE"
        };
        assert_eq!(
            connector["Status"].as_str(),
            Some(expected_status),
            "case {}: a connector read {elapsed:?} after CreateConnector (delay \
             {CONNECTOR_ACTIVATION_DELAY:?}) must be {expected_status}, got {connector}",
            case.name
        );
        assert!(
            connector["CreatedAt"].is_number(),
            "case {}: CreatedAt must be set, got {connector}",
            case.name
        );
        assert_eq!(
            connector["CreatedAt"], connector["UpdatedAt"],
            "case {}: an untouched connector has UpdatedAt == CreatedAt, got {connector}",
            case.name
        );
        let endpoint = connector["CertificateEnrollmentPolicyServerEndpoint"]
            .as_str()
            .unwrap_or_default();
        let connector_id = &arn[prefix.len()..];
        assert!(
            endpoint.contains(connector_id) && endpoint.contains(REGION),
            "case {}: the enrollment endpoint must name the connector and its region, got {endpoint:?}",
            case.name
        );
        assert!(
            connector["StatusReason"].is_null(),
            "case {}: a connector that did not fail carries no StatusReason, got {connector}",
            case.name
        );
    }
}

#[tokio::test]
async fn get_connector_on_an_arn_that_was_never_created_is_resource_not_found() {
    let router = router();
    // Well-formed connector ARNs the service never minted: a different uuid, a
    // different region, and one created-then-deleted below in its own case.
    let unknown = [
        format!("{}11111111-2222-3333-4444-555555555555", connector_arn_prefix()),
        format!("arn:aws:pca-connector-ad:eu-west-1:{ACCOUNT}:connector/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
        format!("{}00000000-0000-0000-0000-000000000000", connector_arn_prefix()),
    ];

    for arn in &unknown {
        let (status, parsed) = get_connector(&router, arn).await;
        assert_eq!(
            status, 404,
            "case {arn}: an unknown connector ARN is a 404, got {status} / {parsed}"
        );
        assert_eq!(
            parsed["__code"].as_str(),
            Some("ResourceNotFoundException"),
            "case {arn}: expected ResourceNotFoundException, got {parsed}"
        );
        assert_eq!(
            parsed["ResourceType"].as_str(),
            Some("Connector"),
            "case {arn}: the refusal must name the resource type, got {parsed}"
        );
    }
}

#[tokio::test]
async fn a_connector_settles_from_creating_to_active_after_its_activation_delay() {
    let router = router();
    // The CREATING observation is owed only while less than the activation delay
    // has elapsed since the create; on a loaded machine the two dispatches can
    // themselves take longer than that, so the case asserts against the time the
    // round trip actually took rather than flaking on a slow build host.
    let started = std::time::Instant::now();
    let arn = create_connector(&router, &CONNECTOR_CASES[0].body()).await;

    let (_, immediately) = get_connector(&router, &arn).await;
    let elapsed = started.elapsed();
    let expected_status = if elapsed < CONNECTOR_ACTIVATION_DELAY {
        "CREATING"
    } else {
        "ACTIVE"
    };
    assert_eq!(
        immediately["Connector"]["Status"].as_str(),
        Some(expected_status),
        "a connector read {elapsed:?} after CreateConnector (delay {CONNECTOR_ACTIVATION_DELAY:?}) \
         must be {expected_status}, got {immediately}"
    );

    tokio::time::sleep(PAST_ACTIVATION).await;

    let (_, settled) = get_connector(&router, &arn).await;
    assert_eq!(
        settled["Connector"]["Status"].as_str(),
        Some("ACTIVE"),
        "after the activation delay the connector must be ACTIVE, got {settled}"
    );
    assert_eq!(
        settled["Connector"]["Arn"], immediately["Connector"]["Arn"],
        "settling must not change the connector's identity, got {settled}"
    );
}

#[tokio::test]
async fn list_connectors_pages_through_every_connector_once_in_creation_order() {
    let router = router();

    // Six connectors, so page sizes below span "divides evenly", "leaves a
    // remainder" and "larger than the collection".
    let mut created = Vec::new();
    for index in 0..6 {
        let case = &CONNECTOR_CASES[index % CONNECTOR_CASES.len()];
        created.push(create_connector(&router, &case.body()).await);
    }

    for page_size in [1usize, 2, 4, 6, 1000] {
        let mut seen = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let query = match &token {
                Some(token) => format!(
                    "/connectors?MaxResults={page_size}&NextToken={}",
                    encode_label(token)
                ),
                None => format!("/connectors?MaxResults={page_size}"),
            };
            let (status, parsed) = call(&router, "GET", &query, None).await;
            assert_eq!(
                status, 200,
                "case page_size={page_size}: ListConnectors must answer 200, got {status} / {parsed}"
            );
            let page = parsed["Connectors"]
                .as_array()
                .unwrap_or_else(|| panic!("case page_size={page_size}: no Connectors: {parsed}"));
            assert!(
                !page.is_empty() && page.len() <= page_size,
                "case page_size={page_size}: a page holds 1..={page_size} items, got {}",
                page.len()
            );
            for summary in page {
                seen.push(
                    summary["Arn"]
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!("case page_size={page_size}: summary without Arn: {summary}")
                        })
                        .to_string(),
                );
            }
            pages += 1;
            assert!(
                pages <= created.len() + 1,
                "case page_size={page_size}: paging did not terminate after {pages} pages"
            );
            match parsed["NextToken"].as_str() {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            seen, created,
            "case page_size={page_size}: paging must yield every connector exactly once in creation order"
        );
        assert_eq!(
            seen.iter().collect::<BTreeSet<_>>().len(),
            created.len(),
            "case page_size={page_size}: paging must not duplicate a connector, got {seen:?}"
        );
    }
}

#[tokio::test]
async fn list_connectors_refuses_a_next_token_it_never_issued() {
    let router = router();
    create_connector(&router, &CONNECTOR_CASES[0].body()).await;

    // Every value here satisfies the modelled `NextToken` pattern (base64url
    // characters in groups of four), so the refusal comes from the handler's
    // cursor check and not from the boundary validator. The rows span the ways a
    // token can be foreign: a value with no service tag at all, the tag alone,
    // the tag with a cursor field of the wrong width, the tag with a field that
    // is not hex, a wrong-case tag, and a bare cursor field — the shape a
    // service without a tagged token would silently honour.
    let foreign = [
        "YWJj",
        "AAAA",
        "X-_a",
        "Nw==",
        "pcad",
        "pcad00000000000000010000",
        "pcadzzzzzzzzzzzzzzzz",
        "pcaD0000000000000001",
        "0000000000000001",
    ];
    for token in foreign {
        let (status, parsed) = call(
            &router,
            "GET",
            &format!("/connectors?NextToken={}", encode_label(token)),
            None,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {token}: a token this service never issued is a 400, got {status} / {parsed}"
        );
        assert_eq!(
            parsed["__code"].as_str(),
            Some("ValidationException"),
            "case {token}: expected ValidationException, got {parsed}"
        );
        // One wording for this error class across this service's list operations
        // (`state::pagination_token_message`): the fixed sentence, the operation
        // from the request, then the schema the presented token declares. The
        // layer that reads the token keys it at the lowercase `message` member.
        assert!(
            parsed["message"]
                .as_str()
                .unwrap_or_default()
                .starts_with("Pagination token exception in operation 'ListConnectors': "),
            "case {token}: the refusal must read as a pagination-token failure of this \
             operation, got {parsed}"
        );
    }
}

/// One row of the template table: the definition union version, which fixes the
/// `PolicySchema` the service must report.
struct TemplateCase {
    name: &'static str,
    expected_policy_schema: i64,
    definition: fn() -> Value,
}

fn certificate_validity() -> Value {
    // `ValidityPeriodType` values come from the modelled enum
    // (`VALIDITY_PERIOD_TYPE_VALUES`).
    json!({
        "ValidityPeriod": { "PeriodType": "YEARS", "Period": 1 },
        "RenewalPeriod": { "PeriodType": "WEEKS", "Period": 6 },
    })
}

fn template_v2() -> Value {
    json!({ "TemplateV2": {
        "CertificateValidity": certificate_validity(),
        "PrivateKeyAttributes": { "MinimalKeyLength": 2048, "KeySpec": "KEY_EXCHANGE" },
        "PrivateKeyFlags": { "ClientVersion": "WINDOWS_SERVER_2016" },
        "EnrollmentFlags": { "NoSecurityExtension": true },
        "SubjectNameFlags": { "SanRequireDirectoryGuid": true },
        "GeneralFlags": { "MachineType": true },
        "Extensions": { "KeyUsage": { "UsageFlags": { "KeyEncipherment": true, "DigitalSignature": true } } },
    }})
}

fn template_v3() -> Value {
    json!({ "TemplateV3": {
        "CertificateValidity": certificate_validity(),
        "PrivateKeyAttributes": {
            "MinimalKeyLength": 2048,
            "KeySpec": "KEY_EXCHANGE",
            "KeyUsageProperty": { "PropertyType": "ALL" },
            "Algorithm": "RSA",
        },
        "PrivateKeyFlags": { "ClientVersion": "WINDOWS_SERVER_2016" },
        "EnrollmentFlags": { "NoSecurityExtension": true },
        "SubjectNameFlags": { "SanRequireDirectoryGuid": true },
        "GeneralFlags": { "MachineType": true },
        "HashAlgorithm": "SHA256",
        "Extensions": { "KeyUsage": { "UsageFlags": { "KeyEncipherment": true } } },
    }})
}

fn template_v4() -> Value {
    json!({ "TemplateV4": {
        "CertificateValidity": certificate_validity(),
        "PrivateKeyAttributes": { "MinimalKeyLength": 2048, "KeySpec": "KEY_EXCHANGE" },
        "PrivateKeyFlags": { "ClientVersion": "WINDOWS_SERVER_2016" },
        "EnrollmentFlags": { "NoSecurityExtension": true },
        "SubjectNameFlags": { "SanRequireDirectoryGuid": true },
        "GeneralFlags": { "MachineType": true },
        "Extensions": { "KeyUsage": { "UsageFlags": { "KeyEncipherment": true } } },
    }})
}

/// The `TemplateDefinition` union's members are `TemplateV2`, `TemplateV3` and
/// `TemplateV4`; the schema version AWS reports is the version of the member
/// supplied, so the table covers every one of them.
const TEMPLATE_CASES: &[TemplateCase] = &[
    TemplateCase {
        name: "TemplateV2",
        expected_policy_schema: 2,
        definition: template_v2,
    },
    TemplateCase {
        name: "TemplateV3",
        expected_policy_schema: 3,
        definition: template_v3,
    },
    TemplateCase {
        name: "TemplateV4",
        expected_policy_schema: 4,
        definition: template_v4,
    },
];

async fn create_template(
    router: &MultiServiceRouter,
    connector_arn: &str,
    name: &str,
    definition: &Value,
) -> (u16, Value) {
    let body = json!({
        "ConnectorArn": connector_arn,
        "Name": name,
        "Definition": definition,
    });
    call(router, "POST", "/templates", Some(&body)).await
}

#[tokio::test]
async fn create_template_persists_the_definition_and_get_template_reads_it_back() {
    let router = router();
    let connector_arn = create_connector(&router, &CONNECTOR_CASES[0].body()).await;

    for case in TEMPLATE_CASES {
        let definition = (case.definition)();
        let template_name = format!("Template-{}", case.name);
        let (status, parsed) =
            create_template(&router, &connector_arn, &template_name, &definition).await;
        assert_eq!(
            status, 200,
            "case {}: CreateTemplate must answer the modelled 200, got {status} / {parsed}",
            case.name
        );
        let template_arn = parsed["TemplateArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {}: no TemplateArn: {parsed}", case.name))
            .to_string();
        let child_prefix = format!("{connector_arn}/template/");
        assert!(
            template_arn.starts_with(&child_prefix)
                && is_uuid(&template_arn[child_prefix.len()..]),
            "case {}: expected `<connectorArn>/template/<uuid>`, got {template_arn:?}",
            case.name
        );

        let (status, parsed) = call(
            &router,
            "GET",
            &format!("/templates/{}", encode_label(&template_arn)),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {}: expected GetTemplate 200, got {status} / {parsed}",
            case.name
        );
        let template = &parsed["Template"];
        assert_eq!(
            template["Arn"].as_str(),
            Some(template_arn.as_str()),
            "case {}: GetTemplate must report the ARN it was asked for, got {template}",
            case.name
        );
        assert_eq!(
            template["ConnectorArn"].as_str(),
            Some(connector_arn.as_str()),
            "case {}: the template must name its parent connector, got {template}",
            case.name
        );
        assert_eq!(
            template["Name"].as_str(),
            Some(template_name.as_str()),
            "case {}: Name must round-trip, got {template}",
            case.name
        );
        assert_eq!(
            template["Definition"], definition,
            "case {}: Definition must come back exactly as supplied, got {template}",
            case.name
        );
        assert_eq!(
            template["PolicySchema"].as_i64(),
            Some(case.expected_policy_schema),
            "case {}: PolicySchema is the definition's own version, got {template}",
            case.name
        );
        assert_eq!(
            template["Status"].as_str(),
            Some("ACTIVE"),
            "case {}: TemplateStatus models only ACTIVE/DELETING, so a new template is ACTIVE, got {template}",
            case.name
        );
        assert_eq!(
            template["Revision"],
            json!({ "MajorRevision": 1, "MinorRevision": 0 }),
            "case {}: a never-updated template sits at its first revision, got {template}",
            case.name
        );
        let oid = template["ObjectIdentifier"].as_str().unwrap_or_default();
        assert!(
            oid.starts_with(&format!("{AD_TEMPLATE_OID_ARC}.")),
            "case {}: the template OID must sit under the AD template arc, got {oid:?}",
            case.name
        );
        assert!(
            oid.split('.').all(|part| !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())),
            "case {}: an OID is dot-separated non-negative integers, got {oid:?}",
            case.name
        );
        assert!(
            template["CreatedAt"].is_number() && template["UpdatedAt"].is_number(),
            "case {}: CreatedAt/UpdatedAt must be set, got {template}",
            case.name
        );
    }
}

#[tokio::test]
async fn template_names_are_unique_per_connector_and_free_on_another_connector() {
    let router = router();
    let first = create_connector(&router, &CONNECTOR_CASES[0].body()).await;
    let second = create_connector(&router, &CONNECTOR_CASES[1].body()).await;

    for case in TEMPLATE_CASES {
        let definition = (case.definition)();
        let name = format!("Shared-{}", case.name);

        let (status, parsed) = create_template(&router, &first, &name, &definition).await;
        assert_eq!(
            status, 200,
            "case {}: the first use of a name must succeed, got {status} / {parsed}",
            case.name
        );

        let (status, parsed) = create_template(&router, &first, &name, &definition).await;
        assert_eq!(
            status, 409,
            "case {}: a duplicate name on the same connector is a 409, got {status} / {parsed}",
            case.name
        );
        assert_eq!(
            parsed["__code"].as_str(),
            Some("ConflictException"),
            "case {}: expected ConflictException, got {parsed}",
            case.name
        );

        let (status, parsed) = create_template(&router, &second, &name, &definition).await;
        assert_eq!(
            status, 200,
            "case {}: the same name on another connector is free, got {status} / {parsed}",
            case.name
        );
    }
}

#[tokio::test]
async fn create_template_on_a_connector_that_does_not_exist_is_resource_not_found() {
    let router = router();
    let unknown = format!(
        "{}99999999-8888-7777-6666-555555555555",
        connector_arn_prefix()
    );

    for case in TEMPLATE_CASES {
        let (status, parsed) =
            create_template(&router, &unknown, "Orphan", &(case.definition)()).await;
        assert_eq!(
            status, 404,
            "case {}: an unknown ConnectorArn is a 404, got {status} / {parsed}",
            case.name
        );
        assert_eq!(
            parsed["__code"].as_str(),
            Some("ResourceNotFoundException"),
            "case {}: expected ResourceNotFoundException, got {parsed}",
            case.name
        );
    }
}

#[tokio::test]
async fn delete_connector_removes_it_with_its_templates_and_stays_idempotent() {
    let router = router();
    let doomed = create_connector(&router, &CONNECTOR_CASES[0].body()).await;
    let survivor = create_connector(&router, &CONNECTOR_CASES[2].body()).await;

    let mut doomed_templates = Vec::new();
    for case in TEMPLATE_CASES {
        let (_, parsed) = create_template(
            &router,
            &doomed,
            &format!("Doomed-{}", case.name),
            &(case.definition)(),
        )
        .await;
        doomed_templates.push(parsed["TemplateArn"].as_str().unwrap_or_default().to_string());
    }
    let (_, survivor_template) = create_template(
        &router,
        &survivor,
        "Survivor",
        &(TEMPLATE_CASES[0].definition)(),
    )
    .await;
    let survivor_template_arn = survivor_template["TemplateArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // Two deletes of the same ARN, and one of an ARN that never existed: all 202.
    let never_existed = format!(
        "{}12121212-3434-5656-7878-909090909090",
        connector_arn_prefix()
    );
    for (attempt, arn) in [&doomed, &doomed, &never_existed].iter().enumerate() {
        let (status, parsed) = call(
            &router,
            "DELETE",
            &format!("/connectors/{}", encode_label(arn)),
            None,
        )
        .await;
        assert_eq!(
            status, 202,
            "case attempt {attempt} on {arn}: @idempotent DeleteConnector always answers 202, got {status} / {parsed}"
        );
    }

    let (status, parsed) = get_connector(&router, &doomed).await;
    assert_eq!(
        status, 404,
        "a deleted connector must read back as 404, got {status} / {parsed}"
    );

    for template_arn in &doomed_templates {
        let (status, parsed) = call(
            &router,
            "GET",
            &format!("/templates/{}", encode_label(template_arn)),
            None,
        )
        .await;
        assert_eq!(
            status, 404,
            "case {template_arn}: a deleted connector's templates go with it, got {status} / {parsed}"
        );
    }

    let (status, parsed) = call(
        &router,
        "GET",
        &format!("/templates/{}", encode_label(&survivor_template_arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "another connector's template must survive, got {status} / {parsed}"
    );

    let (status, listed) = call(&router, "GET", "/connectors", None).await;
    assert_eq!(status, 200, "ListConnectors must answer 200, got {listed}");
    let arns = listed_arns(&listed);
    assert_eq!(
        arns,
        vec![survivor.clone()],
        "after the delete only the surviving connector is listed, got {listed}"
    );
}

#[tokio::test]
async fn a_repeated_client_token_returns_the_first_resource_rather_than_a_second_one() {
    let router = router();

    // Connector: the same token twice must name one connector, and a different
    // token must mint another.
    let mut body = CONNECTOR_CASES[0].body();
    body["ClientToken"] = json!("token-connector-1");
    let first = create_connector(&router, &body).await;
    let repeat = create_connector(&router, &body).await;
    assert_eq!(
        first, repeat,
        "case ClientToken reuse: a retried CreateConnector must return the first ARN, got {first} then {repeat}"
    );

    let mut other = CONNECTOR_CASES[1].body();
    other["ClientToken"] = json!("token-connector-2");
    let second = create_connector(&router, &other).await;
    assert_ne!(
        first, second,
        "case distinct ClientTokens: a new token must mint a new connector, got {second}"
    );

    let (_, listed) = call(&router, "GET", "/connectors", None).await;
    let arns = listed_arns(&listed);
    assert_eq!(
        arns,
        vec![first.clone(), second.clone()],
        "two tokens over three calls must leave exactly two connectors, got {listed}"
    );

    // Template: the same rule, on the child resource.
    for case in TEMPLATE_CASES {
        let definition = (case.definition)();
        let body = json!({
            "ConnectorArn": first,
            "Name": format!("Idempotent-{}", case.name),
            "Definition": definition,
            "ClientToken": format!("token-template-{}", case.name),
        });
        let (status, created) = call(&router, "POST", "/templates", Some(&body)).await;
        assert_eq!(
            status, 200,
            "case {}: CreateTemplate must answer 200, got {status} / {created}",
            case.name
        );
        let (status, retried) = call(&router, "POST", "/templates", Some(&body)).await;
        assert_eq!(
            status, 200,
            "case {}: a retried CreateTemplate must answer 200, got {status} / {retried}",
            case.name
        );
        assert_eq!(
            created["TemplateArn"], retried["TemplateArn"],
            "case {}: a retried CreateTemplate must return the first ARN, got {created} then {retried}",
            case.name
        );

        // The same token against a DIFFERENT connector addresses a different
        // resource, so it must mint a new template under that connector rather
        // than answer with the first connector's ARN.
        let mut elsewhere = body.clone();
        elsewhere["ConnectorArn"] = json!(second);
        let (status, other) = call(&router, "POST", "/templates", Some(&elsewhere)).await;
        assert_eq!(
            status, 200,
            "case {}: the same token on another connector must succeed, got {status} / {other}",
            case.name
        );
        assert_ne!(
            created["TemplateArn"], other["TemplateArn"],
            "case {}: a token reused against another connector must not return the first connector's template, got {other}",
            case.name
        );
        let other_arn = other["TemplateArn"].as_str().unwrap_or_default();
        assert!(
            other_arn.starts_with(&format!("{second}/template/")),
            "case {}: the new template must be a child of the connector addressed, got {other_arn:?}",
            case.name
        );
    }
}

#[tokio::test]
async fn the_connector_store_is_bounded_and_refuses_past_its_quota() {
    let router = router();
    // The bound is on connectors, and several connectors may share one directory,
    // so the prerequisite registrations are established once up front.
    for case in CONNECTOR_CASES {
        register_directory(&router, case.directory_id).await;
    }

    for index in 0..MAX_CONNECTORS {
        let case = &CONNECTOR_CASES[index % CONNECTOR_CASES.len()];
        let (status, parsed) = call(&router, "POST", "/connectors", Some(&case.body())).await;
        assert_eq!(
            status, 202,
            "case connector {index}: every create up to the bound must succeed, got {status} / {parsed}"
        );
    }

    let (status, parsed) = call(
        &router,
        "POST",
        "/connectors",
        Some(&CONNECTOR_CASES[0].body()),
    )
    .await;
    assert_eq!(
        status, 402,
        "case one past the bound: the create must be refused with the modelled quota status, got {status} / {parsed}"
    );
    assert_eq!(
        parsed["__code"].as_str(),
        Some("ServiceQuotaExceededException"),
        "case one past the bound: expected ServiceQuotaExceededException, got {parsed}"
    );
    assert_eq!(
        parsed["ResourceType"].as_str(),
        Some("Connector"),
        "case one past the bound: the refusal must name the resource type, got {parsed}"
    );

    // The refusal did not grow the store: exactly the bound is held.
    let (_, listed) = call(
        &router,
        "GET",
        &format!("/connectors?MaxResults={MAX_CONNECTORS}"),
        None,
    )
    .await;
    assert_eq!(
        listed_arns(&listed).len(),
        MAX_CONNECTORS,
        "a refused create must not have been stored, got {} connectors",
        listed_arns(&listed).len()
    );
}

#[tokio::test]
async fn the_template_store_is_bounded_per_connector_and_refuses_past_its_quota() {
    let router = router();
    let connector_arn = create_connector(&router, &CONNECTOR_CASES[0].body()).await;
    let other_arn = create_connector(&router, &CONNECTOR_CASES[1].body()).await;
    let definition = (TEMPLATE_CASES[0].definition)();

    for index in 0..MAX_TEMPLATES_PER_CONNECTOR {
        let (status, parsed) = create_template(
            &router,
            &connector_arn,
            &format!("Bounded-{index}"),
            &definition,
        )
        .await;
        assert_eq!(
            status, 200,
            "case template {index}: every create up to the bound must succeed, got {status} / {parsed}"
        );
    }

    let (status, parsed) =
        create_template(&router, &connector_arn, "OnePastTheBound", &definition).await;
    assert_eq!(
        status, 402,
        "case one past the bound: the create must be refused with the modelled quota status, got {status} / {parsed}"
    );
    assert_eq!(
        parsed["__code"].as_str(),
        Some("ServiceQuotaExceededException"),
        "case one past the bound: expected ServiceQuotaExceededException, got {parsed}"
    );

    // The bound is per connector, so another connector is unaffected.
    let (status, parsed) =
        create_template(&router, &other_arn, "FirstOnAnotherConnector", &definition).await;
    assert_eq!(
        status, 200,
        "case another connector: the per-connector bound must not spill over, got {status} / {parsed}"
    );
}

// --- The group access control entries one template carries -------------------
//
// An entry says whether ONE Active Directory group may enroll and autoenroll
// certificates with ONE template, and is addressed by that pair: the template's
// ARN and the group's security identifier. It has no ARN of its own and the
// create's modelled output is `Unit`, so every case below reads the entry back
// through `GetTemplateGroupAccessControlEntry` rather than off the create.

/// One row of the entry table: inputs that differ along what the rule generalises
/// over (the shape and length of the group identifier, the display name, and which
/// of the two access rights the request carries).
struct EntryCase {
    name: &'static str,
    /// `GroupSecurityIdentifier`, modelled `len 7..256` with pattern
    /// `^S-[0-9]-([0-9]+-){1,14}[0-9]+$`
    /// (`smithy_query(query="operation",operation="CreateTemplateGroupAccessControlEntry")`).
    group_security_identifier: String,
    /// `GroupDisplayName`, modelled `DisplayName` `len 0..256` with pattern
    /// `^[\x20-\x7E]+$`.
    group_display_name: String,
    /// `AccessRights$Enroll` / `$AutoEnroll`, both optional, each one of the modelled
    /// `AccessRight` enum's values (`ACCESS_RIGHT_VALUES`: `ALLOW`, `DENY`).
    enroll: Option<&'static str>,
    auto_enroll: Option<&'static str>,
}

impl EntryCase {
    fn body(&self) -> Value {
        let mut rights = json!({});
        if let Some(enroll) = self.enroll {
            rights["Enroll"] = json!(enroll);
        }
        if let Some(auto_enroll) = self.auto_enroll {
            rights["AutoEnroll"] = json!(auto_enroll);
        }
        json!({
            "GroupSecurityIdentifier": self.group_security_identifier,
            "GroupDisplayName": self.group_display_name,
            "AccessRights": rights,
        })
    }
}

/// The rows span both published bounds of the two string members — the shortest
/// identifier the SID pattern admits (which is also its modelled minimum length of
/// 7) and a display name at the modelled maximum of 256 characters — the maximum
/// number of sub-authorities the pattern admits (14), and every combination of the
/// two access rights a request can carry.
fn entry_cases() -> Vec<EntryCase> {
    vec![
        EntryCase {
            name: "the shortest SID the pattern admits, enroll allowed and autoenroll denied",
            group_security_identifier: "S-1-5-1".to_string(),
            group_display_name: "pki-enrollers".to_string(),
            enroll: Some("ALLOW"),
            auto_enroll: Some("DENY"),
        },
        EntryCase {
            name: "a domain SID with a relative id, both rights allowed",
            group_security_identifier: "S-1-5-21-1004336348-1177238915-682003330-1401".to_string(),
            group_display_name: "Domain Computers".to_string(),
            enroll: Some("ALLOW"),
            auto_enroll: Some("ALLOW"),
        },
        EntryCase {
            name: "the maximum 14 sub-authorities, both rights denied",
            group_security_identifier: "S-1-5-21-1-2-3-4-5-6-7-8-9-10-11-12-13".to_string(),
            group_display_name: "x".to_string(),
            enroll: Some("DENY"),
            auto_enroll: Some("DENY"),
        },
        EntryCase {
            name: "only Enroll supplied, a display name at the modelled 256-character maximum",
            group_security_identifier: "S-1-15-2-7".to_string(),
            group_display_name: "D".repeat(256),
            enroll: Some("ALLOW"),
            auto_enroll: None,
        },
        EntryCase {
            name: "only AutoEnroll supplied, printable punctuation in the display name",
            group_security_identifier: "S-2-0-0".to_string(),
            group_display_name: "Trusted Users (read-only) #1".to_string(),
            enroll: None,
            auto_enroll: Some("DENY"),
        },
    ]
}

fn entries_path(template_arn: &str) -> String {
    format!("/templates/{}/accessControlEntries", encode_label(template_arn))
}

fn entry_path(template_arn: &str, group_security_identifier: &str) -> String {
    format!(
        "{}/{}",
        entries_path(template_arn),
        encode_label(group_security_identifier)
    )
}

async fn create_entry(
    router: &MultiServiceRouter,
    template_arn: &str,
    body: &Value,
) -> (u16, Value) {
    call(
        router,
        "POST",
        &entries_path(template_arn),
        Some(body),
    )
    .await
}

async fn get_entry(
    router: &MultiServiceRouter,
    template_arn: &str,
    group_security_identifier: &str,
) -> (u16, Value) {
    call(
        router,
        "GET",
        &entry_path(template_arn, group_security_identifier),
        None,
    )
    .await
}

/// A connector with one template on it, answering the template's ARN: the parent
/// every entry below hangs off.
async fn template_for_entries(router: &MultiServiceRouter, name: &str) -> String {
    let connector_arn = create_connector(router, &CONNECTOR_CASES[0].body()).await;
    let (status, parsed) =
        create_template(router, &connector_arn, name, &(TEMPLATE_CASES[0].definition)()).await;
    assert_eq!(
        status, 200,
        "setup {name}: CreateTemplate must answer the modelled 200, got {status} / {parsed}"
    );
    parsed["TemplateArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup {name}: CreateTemplate returned no TemplateArn: {parsed}"))
        .to_string()
}

/// A template ARN of the modelled child form under `connector_arn` that no
/// `CreateTemplate` ever minted, so it resolves to nothing.
fn unminted_template_arn(connector_arn: &str) -> String {
    format!("{connector_arn}/template/dddddddd-eeee-4fff-8aaa-bbbbbbbbbbbb")
}

#[tokio::test]
async fn create_template_group_access_control_entry_persists_it_and_get_reads_it_back() {
    let router = router();
    let template_arn = template_for_entries(&router, "EntryRoundTrip").await;

    for case in entry_cases() {
        let body = case.body();
        let (status, parsed) = create_entry(&router, &template_arn, &body).await;
        assert_eq!(
            status, 200,
            "case {}: CreateTemplateGroupAccessControlEntry must answer the modelled 200, \
             got {status} / {parsed}",
            case.name
        );
        // The modelled output is `Unit`, so the success carries no members.
        assert!(
            parsed.is_null() || parsed.as_object().is_some_and(serde_json::Map::is_empty),
            "case {}: the modelled output is empty, got {parsed}",
            case.name
        );

        let (status, parsed) =
            get_entry(&router, &template_arn, &case.group_security_identifier).await;
        assert_eq!(
            status, 200,
            "case {}: the created entry must read back, got {status} / {parsed}",
            case.name
        );
        let entry = &parsed["AccessControlEntry"];
        assert_eq!(
            entry["TemplateArn"].as_str(),
            Some(template_arn.as_str()),
            "case {}: the entry must name the template it was created under, got {entry}",
            case.name
        );
        assert_eq!(
            entry["GroupSecurityIdentifier"].as_str(),
            Some(case.group_security_identifier.as_str()),
            "case {}: the entry must report the identifier it was addressed by, got {entry}",
            case.name
        );
        assert_eq!(
            entry["GroupDisplayName"].as_str(),
            Some(case.group_display_name.as_str()),
            "case {}: GroupDisplayName must be what the create persisted, got {entry}",
            case.name
        );
        assert_eq!(
            entry["AccessRights"], body["AccessRights"],
            "case {}: AccessRights must round-trip exactly as supplied, got {entry}",
            case.name
        );
        assert!(
            entry["CreatedAt"].is_number(),
            "case {}: CreatedAt must be set, got {entry}",
            case.name
        );
        assert_eq!(
            entry["CreatedAt"], entry["UpdatedAt"],
            "case {}: an untouched entry has UpdatedAt == CreatedAt, got {entry}",
            case.name
        );
    }
}

#[tokio::test]
async fn an_entry_is_addressed_by_its_template_so_two_templates_hold_their_own() {
    let router = router();
    let connector_arn = create_connector(&router, &CONNECTOR_CASES[0].body()).await;
    let mut template_arns = Vec::new();
    for name in ["EntryScopeFirst", "EntryScopeSecond"] {
        let (_, parsed) =
            create_template(&router, &connector_arn, name, &(TEMPLATE_CASES[0].definition)()).await;
        template_arns.push(
            parsed["TemplateArn"]
                .as_str()
                .unwrap_or_else(|| panic!("setup {name}: no TemplateArn: {parsed}"))
                .to_string(),
        );
    }
    let case = &entry_cases()[0];

    let (status, parsed) = create_entry(&router, &template_arns[0], &case.body()).await;
    assert_eq!(
        status, 200,
        "case the first template: the create must succeed, got {status} / {parsed}"
    );

    // The same group on the other template: a different entry, so nothing reports it
    // until it is created there too.
    let (status, parsed) =
        get_entry(&router, &template_arns[1], &case.group_security_identifier).await;
    assert_eq!(
        status, 404,
        "case the other template: one template's entry is not another's, got {status} / {parsed}"
    );
    let (status, parsed) = create_entry(&router, &template_arns[1], &case.body()).await;
    assert_eq!(
        status, 200,
        "case the other template: the same group may be entered on another template, \
         got {status} / {parsed}"
    );
    for template_arn in &template_arns {
        let (status, parsed) =
            get_entry(&router, template_arn, &case.group_security_identifier).await;
        assert_eq!(
            status, 200,
            "case {template_arn}: each template must hold its own entry, got {status} / {parsed}"
        );
        assert_eq!(
            parsed["AccessControlEntry"]["TemplateArn"].as_str(),
            Some(template_arn.as_str()),
            "case {template_arn}: the entry must name the template it belongs to, got {parsed}"
        );
    }
}

#[tokio::test]
async fn creating_a_group_entry_on_a_template_that_does_not_exist_is_resource_not_found() {
    let router = router();
    let connector_arn = create_connector(&router, &CONNECTOR_CASES[0].body()).await;
    let case = &entry_cases()[1];

    // Two ways a well-formed `TemplateArn` resolves to nothing: a template never
    // minted under a connector this store holds, and one under a connector it does
    // not hold either. Both are refused for the TEMPLATE the request named.
    let absent = [
        (
            "a template never created under an existing connector",
            unminted_template_arn(&connector_arn),
        ),
        (
            "a template under a connector that was never created",
            unminted_template_arn(&format!(
                "{}99999999-8888-4777-a666-555555555555",
                connector_arn_prefix()
            )),
        ),
    ];

    for (name, template_arn) in absent {
        let (status, parsed) = create_entry(&router, &template_arn, &case.body()).await;
        assert_eq!(
            status, 404,
            "case {name}: a template that does not exist is a 404, got {status} / {parsed}"
        );
        assert_eq!(
            parsed["__code"].as_str(),
            Some("ResourceNotFoundException"),
            "case {name}: expected ResourceNotFoundException, got {parsed}"
        );
        let expected = format!("Template was not found: {template_arn}.");
        assert_eq!(
            parsed["Message"].as_str(),
            Some(expected.as_str()),
            "case {name}: expected message {expected:?}, got {parsed}"
        );
        assert_eq!(
            parsed["ResourceId"].as_str(),
            Some(template_arn.as_str()),
            "case {name}: the refusal must echo the identifier as supplied, got {parsed}"
        );
        assert_eq!(
            parsed["ResourceType"].as_str(),
            Some("Template"),
            "case {name}: the refusal must name the resource type, got {parsed}"
        );

        // The refused create stored nothing: the same read is still a 404.
        let (status, parsed) =
            get_entry(&router, &template_arn, &case.group_security_identifier).await;
        assert_eq!(
            status, 404,
            "case {name}: a refused create must persist nothing, got {status} / {parsed}"
        );
    }
}

#[tokio::test]
async fn reading_a_group_entry_a_template_does_not_carry_is_resource_not_found() {
    let router = router();
    let template_arn = template_for_entries(&router, "EntryMissing").await;
    let created = &entry_cases()[0];
    let (status, parsed) = create_entry(&router, &template_arn, &created.body()).await;
    assert_eq!(
        status, 200,
        "setup: the create must succeed, got {status} / {parsed}"
    );

    // A template this store holds, and groups it carries no entry for: the resource
    // the request addressed is the ENTRY, named by the identifier it supplied.
    for absent in ["S-1-5-32-544", "S-1-5-21-9999999999-1-2-3"] {
        let (status, parsed) = get_entry(&router, &template_arn, absent).await;
        assert_eq!(
            status, 404,
            "case {absent}: a group with no entry is a 404, got {status} / {parsed}"
        );
        assert_eq!(
            parsed["__code"].as_str(),
            Some("ResourceNotFoundException"),
            "case {absent}: expected ResourceNotFoundException, got {parsed}"
        );
        let expected = format!("AccessControlEntry was not found: {absent}.");
        assert_eq!(
            parsed["Message"].as_str(),
            Some(expected.as_str()),
            "case {absent}: expected message {expected:?}, got {parsed}"
        );
        assert_eq!(
            parsed["ResourceId"].as_str(),
            Some(absent),
            "case {absent}: the refusal must echo the identifier as supplied, got {parsed}"
        );
        assert_eq!(
            parsed["ResourceType"].as_str(),
            Some("AccessControlEntry"),
            "case {absent}: the refusal must name the resource type, got {parsed}"
        );
    }
}

#[tokio::test]
async fn a_repeated_create_of_one_group_entry_answers_the_recorded_entry_unchanged() {
    let router = router();
    let template_arn = template_for_entries(&router, "EntryIdempotent").await;
    let case = &entry_cases()[1];

    // `@idempotent`: the template and the group identify at most one entry, so the
    // same create repeated — with and without a `ClientToken`, and with a second,
    // different token — is the same 200 and leaves the entry as it was.
    let mut bodies = [case.body(), case.body(), case.body()];
    bodies[1]["ClientToken"] = json!("entry-token-1");
    bodies[2]["ClientToken"] = json!("entry-token-2");

    let mut first_created_at = Value::Null;
    for (attempt, body) in bodies.iter().enumerate() {
        let (status, parsed) = create_entry(&router, &template_arn, body).await;
        assert_eq!(
            status, 200,
            "case attempt {attempt}: every repeat answers the modelled 200, got {status} / {parsed}"
        );
        let (status, parsed) =
            get_entry(&router, &template_arn, &case.group_security_identifier).await;
        assert_eq!(
            status, 200,
            "case attempt {attempt}: the entry must still read back, got {status} / {parsed}"
        );
        let entry = &parsed["AccessControlEntry"];
        if attempt == 0 {
            first_created_at = entry["CreatedAt"].clone();
        }
        assert_eq!(
            entry["CreatedAt"], first_created_at,
            "case attempt {attempt}: a repeat must not mint a second entry, got {entry}"
        );
        assert_eq!(
            entry["CreatedAt"], entry["UpdatedAt"],
            "case attempt {attempt}: a repeat changes nothing, got {entry}"
        );
        assert_eq!(
            entry["AccessRights"], case.body()["AccessRights"],
            "case attempt {attempt}: the recorded rights must be unchanged, got {entry}"
        );
    }
}

#[tokio::test]
async fn deleting_the_connector_takes_its_templates_group_entries_with_it() {
    let router = router();
    let connector_arn = create_connector(&router, &CONNECTOR_CASES[0].body()).await;
    let (_, parsed) = create_template(
        &router,
        &connector_arn,
        "EntryCascade",
        &(TEMPLATE_CASES[0].definition)(),
    )
    .await;
    let template_arn = parsed["TemplateArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no TemplateArn: {parsed}"))
        .to_string();
    let case = &entry_cases()[0];
    let (status, parsed) = create_entry(&router, &template_arn, &case.body()).await;
    assert_eq!(
        status, 200,
        "setup: the create must succeed, got {status} / {parsed}"
    );

    let (status, parsed) = call(
        &router,
        "DELETE",
        &format!("/connectors/{}", encode_label(&connector_arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 202,
        "DeleteConnector answers the modelled 202, got {status} / {parsed}"
    );

    // The template went with the connector, so the entry it held is gone too — and
    // the refusal is about the template, which is the identifier that now resolves
    // to nothing.
    let (status, parsed) = get_entry(&router, &template_arn, &case.group_security_identifier).await;
    assert_eq!(
        status, 404,
        "a deleted connector's template group entries go with it, got {status} / {parsed}"
    );
    let expected = format!("Template was not found: {template_arn}.");
    assert_eq!(
        parsed["Message"].as_str(),
        Some(expected.as_str()),
        "expected message {expected:?}, got {parsed}"
    );
}

#[tokio::test]
async fn the_group_entry_store_is_bounded_per_template_and_refuses_past_its_quota() {
    let router = router();
    let connector_arn = create_connector(&router, &CONNECTOR_CASES[0].body()).await;
    let mut template_arns = Vec::new();
    for name in ["EntryBounded", "EntryBoundedOther"] {
        let (_, parsed) =
            create_template(&router, &connector_arn, name, &(TEMPLATE_CASES[0].definition)()).await;
        template_arns.push(
            parsed["TemplateArn"]
                .as_str()
                .unwrap_or_else(|| panic!("setup {name}: no TemplateArn: {parsed}"))
                .to_string(),
        );
    }

    /// A distinct group identifier per index, inside the modelled SID pattern.
    fn group(index: usize) -> String {
        format!("S-1-5-21-1004336348-1177238915-682003330-{}", 1000 + index)
    }

    for index in 0..MAX_ACCESS_CONTROL_ENTRIES_PER_TEMPLATE {
        let body = json!({
            "GroupSecurityIdentifier": group(index),
            "GroupDisplayName": format!("group-{index}"),
            "AccessRights": { "Enroll": "ALLOW", "AutoEnroll": "DENY" },
        });
        let (status, parsed) = create_entry(&router, &template_arns[0], &body).await;
        assert_eq!(
            status, 200,
            "case entry {index}: every create up to the bound must succeed, got {status} / {parsed}"
        );
    }

    let past = MAX_ACCESS_CONTROL_ENTRIES_PER_TEMPLATE;
    let body = json!({
        "GroupSecurityIdentifier": group(past),
        "GroupDisplayName": "one-past-the-bound",
        "AccessRights": { "Enroll": "ALLOW" },
    });
    let (status, parsed) = create_entry(&router, &template_arns[0], &body).await;
    assert_eq!(
        status, 402,
        "case one past the bound: the create must be refused with the modelled quota status, \
         got {status} / {parsed}"
    );
    assert_eq!(
        parsed["__code"].as_str(),
        Some("ServiceQuotaExceededException"),
        "case one past the bound: expected ServiceQuotaExceededException, got {parsed}"
    );
    assert_eq!(
        parsed["ResourceType"].as_str(),
        Some("Template"),
        "case one past the bound: the refusal names the resource holding the bound, got {parsed}"
    );
    let (status, parsed) = get_entry(&router, &template_arns[0], &group(past)).await;
    assert_eq!(
        status, 404,
        "case one past the bound: a refused create must not have been stored, \
         got {status} / {parsed}"
    );

    // The bound is per template, so another template is unaffected.
    let (status, parsed) = create_entry(&router, &template_arns[1], &body).await;
    assert_eq!(
        status, 200,
        "case another template: the per-template bound must not spill over, got {status} / {parsed}"
    );
}

// ---------------------------------------------------------------------------
// The address a connector ANNOUNCES its enrollment policy server at
// ---------------------------------------------------------------------------

/// The path a connector's enrollment policy server is announced under, behind the
/// reserved segment every address this emulator announces on its own endpoint sits
/// under and the segment naming this plane: the Region, the connector's id, and the
/// Active Directory policy-provider CEP path AWS publishes such an endpoint under
/// (see `state::enrollment_policy_server_endpoint`).
fn expected_enrollment_endpoint(authority: &str, connector_id: &str) -> String {
    format!(
        "https://{authority}/_aws/cep/{REGION}/{connector_id}\
         /ADPolicyProvider_CEP_Kerberos/service.svc/CEP"
    )
}

/// The `host[:port]` of the base this emulator reports while it is served on no port —
/// which is the position an in-process suite's router is in, and the only address such
/// an emulator has to name. Taken from the framework's own constant, so this case
/// cannot drift from the base a handler reads.
fn unserved_authority() -> String {
    smithy_server_runtime::UNSERVED_URL_BASE
        .trim_start_matches("https://")
        .to_string()
}

/// One row: the host a request REACHED this emulator on, and the authority the
/// connector it creates must therefore be announced at.
struct EnrollmentEndpointCase {
    name: &'static str,
    /// The `Host` (and absolute-form URI authority) the create arrives with.
    host: &'static str,
    /// The authority the announced endpoint must carry, or `None` when the request
    /// addressed a host that names a service, where the address to announce is the
    /// base this emulator itself reports ([`unserved_authority`]).
    expected_authority: Option<&'static str>,
    /// A directory of this row's own, pattern-admitted (`^d-[0-9a-f]{10}$`) and named
    /// by no other case in this suite.
    directory_id: &'static str,
}

/// The rows span what the rule generalises over — the FORM of the address a client
/// reached this emulator on: a loopback name with a port, an IPv4 address with a
/// port, a bracketed IPv6 literal with a port, and the endpoint hosts AWS publishes
/// this service at (which name a service, so they are not an address of this
/// emulator's), in this Region and in another.
const ENROLLMENT_ENDPOINT_CASES: &[EnrollmentEndpointCase] = &[
    EnrollmentEndpointCase {
        name: "a client configured with the local endpoint URL",
        host: "localhost:9297",
        expected_authority: Some("localhost:9297"),
        directory_id: "d-aaaa000011",
    },
    EnrollmentEndpointCase {
        name: "a client that dialled the loopback address on another port",
        host: "127.0.0.1:34567",
        expected_authority: Some("127.0.0.1:34567"),
        directory_id: "d-bbbb000022",
    },
    EnrollmentEndpointCase {
        name: "a client that dialled the IPv6 loopback literal",
        host: "[::1]:8443",
        expected_authority: Some("[::1]:8443"),
        directory_id: "d-cccc000033",
    },
    EnrollmentEndpointCase {
        name: "an SDK addressing this service's published endpoint host",
        host: "pca-connector-ad.us-west-2.amazonaws.com",
        expected_authority: None,
        directory_id: "d-dddd000044",
    },
    EnrollmentEndpointCase {
        name: "an SDK addressing this service's published endpoint host in another Region",
        host: "pca-connector-ad.eu-west-1.amazonaws.com",
        expected_authority: None,
        directory_id: "d-eeee000055",
    },
];

impl EnrollmentEndpointCase {
    fn body(&self) -> Value {
        json!({
            "DirectoryId": self.directory_id,
            "CertificateAuthorityArn": format!(
                "arn:aws:acm-pca:{REGION}:{ACCOUNT}:certificate-authority/\
                 66666666-7777-8888-9999-aaaaaaaaaaaa"
            ),
            "VpcInformation": { "SecurityGroupIds": ["sg-0123456789abcdef0"] },
        })
    }

    fn authority(&self) -> String {
        self.expected_authority
            .map_or_else(unserved_authority, str::to_string)
    }
}

/// `CreateConnector` arriving on `host`, answering the ARN it minted. The directory is
/// registered first, which is the prerequisite the create resolves.
async fn create_connector_at_host(
    router: &MultiServiceRouter,
    host: &str,
    body: &Value,
) -> String {
    let directory_id = body["DirectoryId"]
        .as_str()
        .unwrap_or_else(|| panic!("a connector body must carry a DirectoryId: {body}"));
    register_directory(router, directory_id).await;
    let (status, code, parsed) = call_at_host(
        router,
        host,
        ACCOUNT_KEY_ID,
        "POST",
        "/connectors",
        Some(body),
    )
    .await;
    assert_eq!(
        status, 202,
        "host {host}: CreateConnector must answer the modelled 202, \
         got {status} / {code:?} / {parsed}"
    );
    parsed["ConnectorArn"]
        .as_str()
        .unwrap_or_else(|| panic!("host {host}: CreateConnector returned no ConnectorArn: {parsed}"))
        .to_string()
}

/// The `CertificateEnrollmentPolicyServerEndpoint` `ListConnectors` serves for `arn`.
async fn listed_enrollment_endpoint(router: &MultiServiceRouter, arn: &str) -> String {
    let (status, listed) = call(router, "GET", "/connectors", None).await;
    assert_eq!(
        status, 200,
        "ListConnectors must answer 200, got {status} / {listed}"
    );
    listed["Connectors"]
        .as_array()
        .and_then(|summaries| {
            summaries
                .iter()
                .find(|summary| summary["Arn"].as_str() == Some(arn))
                .cloned()
        })
        .unwrap_or_else(|| panic!("ListConnectors served no summary for {arn}: {listed}"))
        ["CertificateEnrollmentPolicyServerEndpoint"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn the_enrollment_endpoint_is_announced_on_an_address_this_emulator_answers_on() {
    let router = router();
    let prefix = connector_arn_prefix();
    let mut announced = BTreeSet::new();

    for case in ENROLLMENT_ENDPOINT_CASES {
        let arn = create_connector_at_host(&router, case.host, &case.body()).await;
        let connector_id = arn[prefix.len()..].to_string();
        let expected = expected_enrollment_endpoint(&case.authority(), &connector_id);

        let (status, read) = get_connector(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {}: GetConnector must read the connector back, got {status} / {read}",
            case.name
        );
        let from_get = read["Connector"]["CertificateEnrollmentPolicyServerEndpoint"]
            .as_str()
            .unwrap_or_default();
        assert_eq!(
            from_get, expected,
            "case {}: GetConnector must announce the enrollment endpoint on the address the \
             create reached this emulator on: expected {expected:?}, got {from_get:?}",
            case.name
        );

        // The same projection of the same record: the list reports what the read does.
        let from_list = listed_enrollment_endpoint(&router, &arn).await;
        assert_eq!(
            from_list, expected,
            "case {}: ListConnectors must serve the same endpoint GetConnector does: \
             expected {expected:?}, got {from_list:?}",
            case.name
        );

        // Whatever address it is announced on, it is never a host AWS publishes: such a
        // host resolves nowhere near this process.
        assert!(
            !from_list.contains("amazonaws.com"),
            "case {}: the announced endpoint must name no AWS-published host, got {from_list:?}",
            case.name
        );
        assert!(
            announced.insert(from_list.clone()),
            "case {}: two connectors must not share one enrollment endpoint, got {from_list:?}",
            case.name
        );
    }

    assert_eq!(
        announced.len(),
        ENROLLMENT_ENDPOINT_CASES.len(),
        "every case must have announced an endpoint of its own, got {announced:?}"
    );
}

#[tokio::test]
async fn the_enrollment_endpoint_is_fixed_when_the_connector_is_created() {
    let router = router();
    let prefix = connector_arn_prefix();
    // Created on the local endpoint URL a client on this machine dials...
    let case = &ENROLLMENT_ENDPOINT_CASES[0];
    let mut body = case.body();
    body["DirectoryId"] = json!("d-ffff000066");
    let arn = create_connector_at_host(&router, case.host, &body).await;
    let expected = expected_enrollment_endpoint(&case.authority(), &arn[prefix.len()..]);

    // ...and read back through requests arriving on every other address: a read reports
    // the connector's own stored endpoint and recomputes nothing, so the value a client
    // was handed does not move under it.
    for other in &ENROLLMENT_ENDPOINT_CASES[1..] {
        let (status, code, read) = call_at_host(
            &router,
            other.host,
            ACCOUNT_KEY_ID,
            "GET",
            &format!("/connectors/{}", encode_label(&arn)),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "read from {}: GetConnector must answer 200, got {status} / {code:?} / {read}",
            other.name
        );
        let from_get = read["Connector"]["CertificateEnrollmentPolicyServerEndpoint"]
            .as_str()
            .unwrap_or_default();
        assert_eq!(
            from_get, expected,
            "read from {}: the endpoint is fixed when the connector is created: \
             expected {expected:?}, got {from_get:?}",
            other.name
        );
    }
}
