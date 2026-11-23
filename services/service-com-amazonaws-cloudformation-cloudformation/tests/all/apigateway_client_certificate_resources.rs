// SPDX-License-Identifier: CC0-1.0

//! `AWS::ApiGateway::ClientCertificate` end to end: the client certificate a stack declares is
//! MINTED in Amazon API Gateway itself, under the identifier the stack publishes.
//!
//! RULE: for a resource type whose owning service mints both the resource's identifier and the
//! material it is made of, the stack creates it through that service's own create call and
//! publishes the identifier THAT service answered as the resource's physical id — so the value
//! a client reads out of `Ref`, of the type's own identifier attribute, of
//! `DescribeStackResource(s)` or of a stack output is one the owning service answers for, the
//! record is discoverable through that service's own collection read with the configuration the
//! declaration asked for, a CHANGED declaration is applied to it through that service's own
//! patch call, and the stack's delete takes it away so the identifier stops
//! resolving. A client certificate is such a resource: `GenerateClientCertificate` takes only a
//! `description` and answers the `clientCertificateId` it minted beside the PEM-encoded
//! certificate it issued and the window that material is valid over, and
//! `GetClientCertificate`, `UpdateClientCertificate`, `DeleteClientCertificate` and a stage's
//! `clientCertificateId` all take that id.
//!
//! The rows differ along what the rule generalises over: whether the declaration states a
//! description at all, whether it states tags beside it, how many certificates one stack
//! declares — and, for the update half, which way a declaration's description moves (replaced,
//! stated anew, dropped) and a change of tags alone.
//!
//! Everything goes over the wire each service speaks through the one router `stack_lifecycle.rs`
//! builds — the CloudFormation calls over its awsQuery wire, the certificate reads over API
//! Gateway's own `restJson1` wire — so what is asserted is what a client that deploys the
//! template and then calls API Gateway observes.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_resources::{apigateway, is_api_gateway_id, member_text};
use crate::stack_lifecycle::{
    call, create_stack, member, output_value, router, settled_stack, xml_text, REGION,
};

/// The resource type under test.
const CLIENT_CERTIFICATE_TYPE: &str = "AWS::ApiGateway::ClientCertificate";

/// How many characters a minted client-certificate id carries: API Gateway assigns a record of
/// the ACCOUNT ten lowercase alphanumeric characters, the width the `RestApi`, `ApiKey` and
/// `VpcLink` ids of that service carry too.
const CLIENT_CERTIFICATE_ID_CHARACTERS: usize = 10;

/// The fixed text a PEM-encoded certificate opens with, as the encoding writes it: the label
/// line and the CRLF that ends it. A member carrying real material begins with exactly this.
const PEM_PREAMBLE: &str = "-----BEGIN CERTIFICATE-----\r\n";

/// The error API Gateway answers for a client certificate it does not hold, and the message
/// that refusal carries — entirely fixed text, with no part taken from the request
/// (`crate::state` of the owning service states it once).
const NOT_FOUND: &str = "NotFoundException";
const NOT_FOUND_MESSAGE: &str = "Invalid client certificate id specified";

/// How long a row gives the stack's delete to reach the owning service before it calls the
/// certificate stuck, and how often it looks. The deadline is far longer than the spans this
/// emulator gives a stack's transitions, so a loaded host cannot fail a case; what the case
/// asserts is the CONDITION (the identifier no longer resolving), never the time it took.
const REMOVAL_DEADLINE: Duration = Duration::from_secs(60);
const REMOVAL_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// One Amazon API Gateway call whose REFUSAL is what the case reads: the status, the error class
/// the `x-amzn-errortype` header names and the body.
///
/// The suite's shared wire helper ([`apigateway`]) answers the status and the body, which is
/// what a 200 read needs; a refusal is judged on its CODE as well, and that travels in a header
/// `restJson1` puts it in, so this is the one call here that keeps the headers.
async fn apigateway_refusal(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
) -> (u16, Option<String>, Value) {
    let host = format!("apigateway.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/apigateway/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::new())
        .expect("a well-formed Amazon API Gateway request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, error_code, parsed)
}

/// `GetClientCertificates` (`GET /clientcertificates`): the certificates the account holds, as
/// the collection's own `item` member carries them.
async fn listed_client_certificates(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let (status, document) = apigateway(router, "GET", "/clientcertificates").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetClientCertificates to answer 200, got {status} with \
         {document}"
    );
    document
        .get("item")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// `GetClientCertificate` (`GET /clientcertificates/{clientCertificateId}`): the certificate one
/// id names.
async fn get_client_certificate(
    router: &MultiServiceRouter,
    client_certificate_id: &str,
) -> (u16, Value) {
    apigateway(
        router,
        "GET",
        &format!("/clientcertificates/{client_certificate_id}"),
    )
    .await
}

/// One `AWS::ApiGateway::ClientCertificate` declaration, as a row writes it.
fn client_certificate_resource(description: Option<&str>, tags: Option<Value>) -> Value {
    let mut properties = serde_json::Map::new();
    if let Some(description) = description {
        properties.insert("Description".to_string(), json!(description));
    }
    if let Some(tags) = tags {
        properties.insert("Tags".to_string(), tags);
    }
    json!({ "Type": CLIENT_CERTIFICATE_TYPE, "Properties": Value::Object(properties) })
}

/// Assert that API Gateway reports every tag the declaration states on the certificate, under
/// the key and the value the declaration spells — the `Tags` property reaching the record
/// rather than being dropped from a create or an update reported as a success.
fn assert_declared_tags(read: &Value, declared: Option<&Value>, case: &str) {
    let Some(entries) = declared.and_then(Value::as_array) else {
        return;
    };
    let reported = read.get("tags").cloned().unwrap_or(Value::Null);
    for entry in entries {
        let key = entry.get("Key").and_then(Value::as_str).unwrap_or_default();
        let value = entry.get("Value").and_then(Value::as_str).unwrap_or_default();
        assert_eq!(
            reported.get(key).and_then(Value::as_str),
            Some(value),
            "case {case:?}: expected the certificate to carry the declared tag \
             {key:?}={value:?}, got {reported}"
        );
    }
}

/// The outputs every row publishes per certificate: what `Ref` resolves to, and the identity
/// attribute the type publishes.
fn published_outputs(logical_ids: &[&str]) -> Value {
    let mut outputs = serde_json::Map::new();
    for logical_id in logical_ids {
        outputs.insert(
            format!("{logical_id}Ref"),
            json!({ "Value": { "Ref": logical_id } }),
        );
        outputs.insert(
            format!("{logical_id}ClientCertificateId"),
            json!({ "Value": { "Fn::GetAtt": [logical_id, "ClientCertificateId"] } }),
        );
    }
    Value::Object(outputs)
}

/// One certificate a row declares, and what the rule says API Gateway must then hold for it.
struct DeclaredCertificate {
    logical_id: &'static str,
    description: Option<&'static str>,
    /// The `Tags` the declaration states, which API Gateway must report on the record.
    tags: Option<Value>,
}

/// One row: a template, and the certificates it declares.
struct CertificateCase {
    case: &'static str,
    stack: &'static str,
    template: String,
    certificates: Vec<DeclaredCertificate>,
}

fn certificate_cases() -> Vec<CertificateCase> {
    vec![
        CertificateCase {
            case: "a certificate with a description and nothing else",
            stack: "agwcc-described",
            template: json!({
                "Resources": {
                    "ClientCertificate": client_certificate_resource(
                        Some("agwcc-described-desc"),
                        None,
                    )
                },
                "Outputs": published_outputs(&["ClientCertificate"])
            })
            .to_string(),
            certificates: vec![DeclaredCertificate {
                logical_id: "ClientCertificate",
                description: Some("agwcc-described-desc"),
                tags: None,
            }],
        },
        CertificateCase {
            case: "a certificate declaring no property at all",
            stack: "agwcc-bare",
            template: json!({
                "Resources": {
                    "ClientCertificate": client_certificate_resource(None, None)
                },
                "Outputs": published_outputs(&["ClientCertificate"])
            })
            .to_string(),
            certificates: vec![DeclaredCertificate {
                logical_id: "ClientCertificate",
                description: None,
                tags: None,
            }],
        },
        CertificateCase {
            case: "a certificate with tags beside its description",
            stack: "agwcc-tagged",
            template: json!({
                "Resources": {
                    "ClientCertificate": client_certificate_resource(
                        Some("agwcc-tagged-desc"),
                        Some(json!([{ "Key": "estate", "Value": "agwcc" }])),
                    )
                },
                "Outputs": published_outputs(&["ClientCertificate"])
            })
            .to_string(),
            certificates: vec![DeclaredCertificate {
                logical_id: "ClientCertificate",
                description: Some("agwcc-tagged-desc"),
                tags: Some(json!([{ "Key": "estate", "Value": "agwcc" }])),
            }],
        },
        CertificateCase {
            case: "two certificates in one stack",
            stack: "agwcc-pair",
            template: json!({
                "Resources": {
                    "Orders": client_certificate_resource(Some("agwcc-pair-orders"), None),
                    "Billing": client_certificate_resource(Some("agwcc-pair-billing"), None)
                },
                "Outputs": published_outputs(&["Orders", "Billing"])
            })
            .to_string(),
            certificates: vec![
                DeclaredCertificate {
                    logical_id: "Orders",
                    description: Some("agwcc-pair-orders"),
                    tags: None,
                },
                DeclaredCertificate {
                    logical_id: "Billing",
                    description: Some("agwcc-pair-billing"),
                    tags: None,
                },
            ],
        },
    ]
}

/// The `PhysicalResourceId` `DescribeStackResource` reports for one logical id.
async fn described_physical_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (status, body) = call(
        router,
        "DescribeStackResource",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackResource({stack}, {logical_id}) answered {status}: {body}"
    );
    xml_text(&body, "PhysicalResourceId").unwrap_or_else(|| {
        panic!("case {case:?}: expected a physical resource id for {logical_id}, got {body}")
    })
}

/// The epoch-seconds instant one member of a reported certificate carries, as the `restJson1`
/// wire writes a timestamp.
fn reported_instant(certificate: &Value, member: &str) -> Option<f64> {
    certificate.get(member).and_then(Value::as_f64)
}

/// Every row: the identifier the stack publishes for a declared client certificate is the one
/// API Gateway minted, that service holds the certificate under it with the material it issued,
/// and every reader of the stack reports the same value.
#[tokio::test]
async fn a_cloudformation_client_certificate_is_minted_under_the_id_the_stack_publishes() {
    let router = router();
    for case in certificate_cases() {
        let name = case.case;
        let (status, body) = create_stack(&router, case.stack, &case.template, &[]).await;
        assert_eq!(
            status, 200,
            "case {name:?}: CreateStack must answer 200, got {status} {body}"
        );
        let described = settled_stack(&router, name, case.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {name:?}: expected the create to complete, got {described}"
        );

        let listed = listed_client_certificates(&router, name).await;
        let mut published: Vec<String> = Vec::new();
        for declared in &case.certificates {
            let logical_id = declared.logical_id;
            let id = described_physical_id(&router, name, case.stack, logical_id).await;
            assert!(
                is_api_gateway_id(&id, CLIENT_CERTIFICATE_ID_CHARACTERS),
                "case {name:?}: expected {logical_id} to report an id of the form API Gateway \
                 mints, got {id:?}"
            );
            assert!(
                !id.contains(case.stack) && !id.contains(logical_id),
                "case {name:?}: expected the published id to be the minted identifier rather \
                 than a name derived from the stack, got {id:?}"
            );
            published.push(id.clone());

            // Every reader of the stack reports the SAME identifier.
            for output in [
                format!("{logical_id}Ref"),
                format!("{logical_id}ClientCertificateId"),
            ] {
                let reported = output_value(&described, &output);
                assert_eq!(
                    reported.as_deref(),
                    Some(id.as_str()),
                    "case {name:?}: expected the output {output:?} to report {id:?}, got \
                     {reported:?}"
                );
            }

            // The owning service LISTS the certificate under that id, carrying the description
            // the declaration asked for — which is what a client filtering
            // `GetClientCertificates` on its description finds.
            let by_id: Vec<&Value> = listed
                .iter()
                .filter(|certificate| {
                    member_text(certificate, "clientCertificateId").as_deref() == Some(id.as_str())
                })
                .collect();
            assert_eq!(
                by_id.len(),
                1,
                "case {name:?}: expected GetClientCertificates to list exactly one certificate \
                 under {id:?}, got {listed:?}"
            );
            assert_eq!(
                member_text(by_id[0], "description").as_deref(),
                declared.description,
                "case {name:?}: expected the listed certificate to report the declared \
                 description, got {:?}",
                by_id[0]
            );

            // ...and answers its own read for that id, reporting the same record with the
            // material API Gateway issued and the window it is valid over.
            let (status, read) = get_client_certificate(&router, &id).await;
            assert_eq!(
                status, 200,
                "case {name:?}: expected GetClientCertificate({id}) to answer 200, got {status} \
                 with {read}"
            );
            assert_eq!(
                member_text(&read, "clientCertificateId").as_deref(),
                Some(id.as_str()),
                "case {name:?}: expected the read to report the published id, got {read}"
            );
            assert_eq!(
                member_text(&read, "description").as_deref(),
                declared.description,
                "case {name:?}: expected the read to report the declared description, got {read}"
            );
            let pem = member_text(&read, "pemEncodedCertificate").unwrap_or_default();
            assert!(
                pem.starts_with(PEM_PREAMBLE),
                "case {name:?}: expected the certificate to carry PEM material, got {pem:?}"
            );
            assert_declared_tags(&read, declared.tags.as_ref(), name);
            let created = reported_instant(&read, "createdDate");
            let expires = reported_instant(&read, "expirationDate");
            match (created, expires) {
                (Some(created), Some(expires)) => assert!(
                    expires > created,
                    "case {name:?}: expected the certificate to expire after it was created, got \
                     created {created:?} and expiring {expires:?}"
                ),
                other => panic!(
                    "case {name:?}: expected the certificate to report both instants, got \
                     {other:?} in {read}"
                ),
            }
        }

        // Two certificates of one stack are two certificates: each carries an identifier of its
        // own, and that service holds one record per declaration.
        let mut unique = published.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            published.len(),
            "case {name:?}: expected one minted identifier per certificate, got {published:?}"
        );
    }
}

/// One row of the update half: the declaration the stack was deployed with, the one it then
/// states, and the description API Gateway must report for the certificate afterwards.
struct UpdateCase {
    case: &'static str,
    stack: &'static str,
    first: Option<&'static str>,
    first_tags: Option<Value>,
    second: Option<&'static str>,
    second_tags: Option<Value>,
}

fn update_cases() -> Vec<UpdateCase> {
    vec![
        UpdateCase {
            case: "a description replaced",
            stack: "agwcc-redescribed",
            first: Some("agwcc-redescribed-before"),
            first_tags: None,
            second: Some("agwcc-redescribed-after"),
            second_tags: None,
        },
        UpdateCase {
            case: "a description stated anew",
            stack: "agwcc-newly-described",
            first: None,
            first_tags: None,
            second: Some("agwcc-newly-described-after"),
            second_tags: None,
        },
        UpdateCase {
            case: "a description the declaration no longer states",
            stack: "agwcc-undescribed",
            first: Some("agwcc-undescribed-before"),
            first_tags: None,
            second: None,
            second_tags: None,
        },
        UpdateCase {
            case: "tags changed beside an unchanged description",
            stack: "agwcc-retagged",
            first: Some("agwcc-retagged-desc"),
            first_tags: Some(json!([{ "Key": "estate", "Value": "before" }])),
            second: Some("agwcc-retagged-desc"),
            second_tags: Some(json!([{ "Key": "estate", "Value": "after" }])),
        },
    ]
}

/// A stack update that states a different declaration for a certificate that EXISTS is applied
/// to that certificate through the patch call API Gateway publishes for one: the stack reports
/// the update complete, the identifier it published goes on naming the same record, and that
/// service reports the description the new declaration states.
#[tokio::test]
async fn a_changed_declaration_redescribes_the_certificate_in_api_gateway() {
    let router = router();
    for row in update_cases() {
        let case = row.case;
        let template = |description: Option<&str>, tags: Option<Value>| {
            json!({
                "Resources": {
                    "ClientCertificate": client_certificate_resource(description, tags)
                },
                "Outputs": published_outputs(&["ClientCertificate"])
            })
            .to_string()
        };
        let (status, body) = create_stack(
            &router,
            row.stack,
            &template(row.first, row.first_tags.clone()),
            &[],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack must answer 200, got {status} {body}"
        );
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {described}"
        );
        let minted = described_physical_id(&router, case, row.stack, "ClientCertificate").await;

        let (status, body) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", row.stack),
                member(
                    "TemplateBody",
                    &template(row.second, row.second_tags.clone()),
                ),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateStack must answer 200, got {status} {body}"
        );
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("UPDATE_COMPLETE"),
            "case {case:?}: expected the update to complete, got {described}"
        );

        // The certificate was re-described, not re-issued: the identifier the stack published
        // still names the record API Gateway minted for the create.
        let after = described_physical_id(&router, case, row.stack, "ClientCertificate").await;
        assert_eq!(
            after, minted,
            "case {case:?}: expected the published id to stay {minted:?}, got {after:?}"
        );
        let (status, read) = get_client_certificate(&router, &minted).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetClientCertificate({minted}) to answer 200, got {status} \
             with {read}"
        );
        assert_eq!(
            member_text(&read, "description").as_deref(),
            row.second,
            "case {case:?}: expected the certificate to report the description the update \
             states, got {read}"
        );
        // ...and the material it was issued with is untouched by the patch.
        let pem = member_text(&read, "pemEncodedCertificate").unwrap_or_default();
        assert!(
            pem.starts_with(PEM_PREAMBLE),
            "case {case:?}: expected the issued material to survive the update, got {pem:?}"
        );
        assert_declared_tags(&read, row.second_tags.as_ref(), case);
    }
}

/// Deleting the stack deletes the client certificate from the service that holds it: the
/// identifier the stack published stops resolving, which is what says the stack's resource
/// really was that service's.
///
/// The rows differ along the same axis the create half does — what the declaration states —
/// because the teardown is of whatever the create put there.
#[tokio::test]
async fn deleting_the_stack_removes_the_client_certificate_from_api_gateway() {
    let rows: &[(&str, &str, Option<&str>)] = &[
        (
            "a described certificate",
            "agwcc-deleted-described",
            Some("agwcc-deleted-desc"),
        ),
        ("a certificate with no description", "agwcc-deleted-bare", None),
    ];
    let router = router();
    for (case, stack, description) in rows {
        let template = json!({
            "Resources": {
                "ClientCertificate": client_certificate_resource(*description, None)
            },
            "Outputs": published_outputs(&["ClientCertificate"])
        })
        .to_string();
        let (status, body) = create_stack(&router, stack, &template, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack must answer 200, got {status} {body}"
        );
        let described = settled_stack(&router, case, stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {described}"
        );
        let id = described_physical_id(&router, case, stack, "ClientCertificate").await;
        let (status, read) = get_client_certificate(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the published id to be one GetClientCertificate accepts, \
             got {status} with {read}"
        );

        let (status, body) = call(&router, "DeleteStack", &[member("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteStack({stack}) answered {status}: {body}"
        );
        // Wait for the CONDITION: the identifier no longer resolving. Every read of the stack
        // is what asks the owning service whether its delete has come through, so the loop
        // takes one each round.
        let deadline = Instant::now() + REMOVAL_DEADLINE;
        let (status, code, document) = loop {
            let answered =
                apigateway_refusal(&router, "GET", &format!("/clientcertificates/{id}")).await;
            if answered.0 == 404 {
                break answered;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the client certificate {id:?} to be removed from the \
                 service that held it, last answer {answered:?}"
            );
            let _ = call(&router, "ListStacks", &[]).await;
            tokio::time::sleep(REMOVAL_POLL_INTERVAL).await;
        };
        assert_eq!(
            status, 404,
            "case {case:?}: expected the removed id to answer 404, got {status} with {document}"
        );
        assert_eq!(
            code.as_deref(),
            Some(NOT_FOUND),
            "case {case:?}: expected the refusal to carry {NOT_FOUND:?}, got {code:?}"
        );
        assert_eq!(
            member_text(&document, "message").as_deref(),
            Some(NOT_FOUND_MESSAGE),
            "case {case:?}: expected the refusal to word the message as API Gateway does, got \
             {document}"
        );
        let listed = listed_client_certificates(&router, case).await;
        let left_behind: Vec<&Value> = listed
            .iter()
            .filter(|certificate| {
                member_text(certificate, "clientCertificateId").as_deref() == Some(id.as_str())
            })
            .collect();
        assert!(
            left_behind.is_empty(),
            "case {case:?}: expected the deleted stack to leave no certificate under {id:?}, got \
             {left_behind:?}"
        );
    }
}
