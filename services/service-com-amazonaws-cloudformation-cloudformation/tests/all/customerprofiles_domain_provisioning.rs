// SPDX-License-Identifier: CC0-1.0

//! The Amazon Connect Customer Profiles domain a stack declares, end to end: the domain exists
//! in that service itself, under the name the stack publishes, it serves the data plane a
//! domain serves, a changed declaration reaches it, and deleting the stack takes it away.
//!
//! RULE: a resource type AWS publishes is one a template may declare, and a declaration of it
//! is DEPLOYED by calling the service that owns the type — so the handle the stack publishes
//! (`Ref`) resolves in that service, the values the type publishes are the ones that service
//! reported, and nothing about the type itself is a template-format error.
//! `customerprofiles_domain_resource_type.rs` pins the admission half of that rule; this suite
//! pins what the stack then really did.
//!
//! The rows differ along what the rule generalises over: whether the declaration NAMES the
//! domain or leaves the name to CloudFormation, where the expiration window sits in its
//! published range (both published bounds and a value between them), whether the declaration
//! carries the encryption key and the dead-letter queue, whether it carries a matching
//! configuration, the template spelling (JSON with long-form intrinsics, YAML with the
//! short-form tags), and how many domains one stack declares.
//!
//! Everything goes over the wire each service speaks through the one router `harness.rs`
//! builds — the CloudFormation calls over its awsQuery wire, the Customer Profiles calls over
//! that service's own `restJson1` wire — so what is asserted is what a client that deploys the
//! template and then calls Customer Profiles observes.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, router, settled_stack_status, stack_event_reasons, REGION,
};
use crate::stack_lifecycle::{describe_stack, output_value};

/// The resource type under test, and the property it takes its name from.
const DOMAIN: &str = "AWS::CustomerProfiles::Domain";
const NAME_PROPERTY: &str = "DomainName";

/// The two published bounds of the expiration window a domain is created with
/// (`expirationDaysInteger`, `@range(min: 1, max: 1098)` in that service's own Smithy model).
const MIN_EXPIRATION_DAYS: i64 = 1;
const MAX_EXPIRATION_DAYS: i64 = 1098;

/// How long a domain's name may be, and the alphabet it is held to: `name`,
/// `@length(min: 1, max: 64)` with pattern `^[a-zA-Z0-9_-]+$` (that service's own model) —
/// which is what a name CloudFormation GENERATES has to be inside.
const MAX_DOMAIN_NAME_CHARACTERS: usize = 64;

/// The error code that service answers for a domain it does not hold, and the status it
/// answers with (the modelled 404 of `GetDomain`).
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u16 = 404;

/// The encryption key and the dead-letter queue one row declares, each of the form its own
/// member admits (`encryptionKey` and `sqsQueueUrl`, plain strings up to 255 characters).
const ENCRYPTION_KEY: &str =
    "arn:aws:kms:us-east-1:000000000000:key/11111111-2222-3333-4444-555555555555";
const DEAD_LETTER_QUEUE_URL: &str =
    "https://sqs.us-east-1.amazonaws.com/000000000000/customer-profiles-dlq";

/// One Amazon Connect Customer Profiles call over the `restJson1` wire its model declares, on
/// the method and URI that operation's own `@http` binding names, signed for `profile` — that
/// model's own `sigv4Name` — in the Region the stack's resources live in.
async fn customer_profiles(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(ToString::to_string).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://profile.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("profile.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/profile/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed Amazon Connect Customer Profiles request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// That service's own read of one domain, by the name the stack published.
async fn get_domain(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    customer_profiles(router, "GET", &format!("/domains/{name}"), None).await
}

/// Deploy `template` as `stack_name` and answer the `DescribeStacks` body of the COMPLETED
/// stack, quoting the stack's own failure reasons when the create did not complete.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = create_stack(router, stack_name, template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack — an {DOMAIN} is a type a template \
         may declare — got {status} with body {body}"
    );
    let reached = settled_stack_status(case, router, REGION, stack_name).await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(router, REGION, stack_name).await
    );
    let (status, described) = describe_stack(router, stack_name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {described}"
    );
    described
}

/// Whether `name` is a name that service's published syntax for a domain admits, which is what
/// a name CloudFormation GENERATES has to be inside.
fn is_a_published_domain_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_DOMAIN_NAME_CHARACTERS
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// What one row expects of one domain the stack declares.
struct Declared {
    /// The output key whose value is the domain's name — what its `Ref` returns.
    name_output: &'static str,
    /// The name the declaration states, or `None` where it leaves the name to CloudFormation.
    declared_name: Option<&'static str>,
    /// The expiration window the declaration states, which the domain must report.
    expiration_days: i64,
    /// The encryption key the declaration states, where it states one.
    encryption_key: Option<&'static str>,
    /// The dead-letter queue the declaration states, where it states one.
    dead_letter_queue_url: Option<&'static str>,
}

/// One row: a template and the domains it declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    declared: Vec<Declared>,
}

fn rows() -> Vec<Row> {
    vec![
        // The issue's own stack: one named domain with an expiration window between the
        // published bounds, every published return value read back, in JSON with long-form
        // intrinsics.
        Row {
            case: "one named domain, every return value read back, JSON spelling",
            stack_name: "customer-profiles-round-trip",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Profiles": {
                        "Type": DOMAIN,
                        "Properties": {
                            NAME_PROPERTY: "cfn-profiles-round-trip",
                            "DefaultExpirationDays": 90,
                        }
                    }
                },
                "Outputs": {
                    "DomainName": {"Value": {"Ref": "Profiles"}},
                    "CreatedAt": {"Value": {"Fn::GetAtt": ["Profiles", "CreatedAt"]}},
                    "LastUpdatedAt": {"Value": {"Fn::GetAtt": ["Profiles", "LastUpdatedAt"]}},
                    "ProfileCount": {
                        "Value": {"Fn::GetAtt": ["Profiles", "Stats.ProfileCount"]}
                    },
                    "ObjectCount": {"Value": {"Fn::GetAtt": ["Profiles", "Stats.ObjectCount"]}},
                    "TotalSize": {"Value": {"Fn::GetAtt": ["Profiles", "Stats.TotalSize"]}},
                    "MeteringProfileCount": {
                        "Value": {"Fn::GetAtt": ["Profiles", "Stats.MeteringProfileCount"]}
                    }
                }
            })
            .to_string(),
            declared: vec![Declared {
                name_output: "DomainName",
                declared_name: Some("cfn-profiles-round-trip"),
                expiration_days: 90,
                encryption_key: None,
                dead_letter_queue_url: None,
            }],
        },
        // No declared name at all, at the LOWEST window the published range admits, carrying
        // the encryption key and the dead-letter queue: the handle the stack publishes is the
        // name CloudFormation generated, and it has to be one that service admits.
        Row {
            case: "an unnamed domain at the minimum window, with a key and a dead-letter queue",
            stack_name: "customer-profiles-generated-name",
            template: json!({
                "Resources": {
                    "Profiles": {
                        "Type": DOMAIN,
                        "Properties": {
                            "DefaultExpirationDays": MIN_EXPIRATION_DAYS,
                            "DefaultEncryptionKey": ENCRYPTION_KEY,
                            "DeadLetterQueueUrl": DEAD_LETTER_QUEUE_URL,
                        }
                    }
                },
                "Outputs": {"DomainName": {"Value": {"Ref": "Profiles"}}}
            })
            .to_string(),
            declared: vec![Declared {
                name_output: "DomainName",
                declared_name: None,
                expiration_days: MIN_EXPIRATION_DAYS,
                encryption_key: Some(ENCRYPTION_KEY),
                dead_letter_queue_url: Some(DEAD_LETTER_QUEUE_URL),
            }],
        },
        // The YAML spelling with short-form tags, at the HIGHEST window the published range
        // admits, carrying a matching configuration and the type's own `Tags`.
        Row {
            case: "the YAML spelling at the maximum window, with a matching configuration",
            stack_name: "customer-profiles-yaml",
            template: format!(
                "\
Resources:
  Profiles:
    Type: {DOMAIN}
    Properties:
      {NAME_PROPERTY}: cfn-profiles-yaml
      DefaultExpirationDays: {MAX_EXPIRATION_DAYS}
      Matching:
        Enabled: true
      Tags:
        - Key: team
          Value: contact-center
Outputs:
  DomainName:
    Value: !Ref Profiles
  CreatedAt:
    Value: !GetAtt Profiles.CreatedAt
"
            ),
            declared: vec![Declared {
                name_output: "DomainName",
                declared_name: Some("cfn-profiles-yaml"),
                expiration_days: MAX_EXPIRATION_DAYS,
                encryption_key: None,
                dead_letter_queue_url: None,
            }],
        },
        // Two domains in one stack, with different windows: each publishes its OWN name and is
        // its own domain in the owning service.
        Row {
            case: "two domains in one stack",
            stack_name: "customer-profiles-pair",
            template: json!({
                "Resources": {
                    "First": {
                        "Type": DOMAIN,
                        "Properties": {
                            NAME_PROPERTY: "cfn-profiles-pair-first",
                            "DefaultExpirationDays": 7,
                        }
                    },
                    "Second": {
                        "Type": DOMAIN,
                        "Properties": {
                            NAME_PROPERTY: "cfn-profiles-pair-second",
                            "DefaultExpirationDays": 365,
                            "RuleBasedMatching": {"Enabled": false},
                        }
                    }
                },
                "Outputs": {
                    "FirstName": {"Value": {"Ref": "First"}},
                    "SecondName": {"Value": {"Ref": "Second"}}
                }
            })
            .to_string(),
            declared: vec![
                Declared {
                    name_output: "FirstName",
                    declared_name: Some("cfn-profiles-pair-first"),
                    expiration_days: 7,
                    encryption_key: None,
                    dead_letter_queue_url: None,
                },
                Declared {
                    name_output: "SecondName",
                    declared_name: Some("cfn-profiles-pair-second"),
                    expiration_days: 365,
                    encryption_key: None,
                    dead_letter_queue_url: None,
                },
            ],
        },
    ]
}

/// One instant in epoch MICROSECONDS, which is the precision a reader of a timestamp carried as
/// a number keeps: two sides of a comparison are the same instant when they agree there, up to
/// the one microsecond two decompositions of an `f64` second can disagree by.
fn microseconds_of(seconds: f64) -> i64 {
    (seconds * 1_000_000.0).round() as i64
}

/// One timestamp a `restJson1` answer carries, in epoch seconds, however that wire wrote it: a
/// JSON protocol carries a timestamp as epoch seconds unless the member's own format makes it a
/// date-time string, and either form denotes the same instant.
fn reported_instant(domain: &Value, member: &str) -> f64 {
    let reported = domain
        .get(member)
        .unwrap_or_else(|| panic!("no {member} in the reported domain {domain}"));
    if let Some(seconds) = reported.as_f64() {
        return seconds;
    }
    let text = reported.as_str().unwrap_or_default();
    aws_smithy_types::DateTime::from_str(text, aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_else(|error| panic!("the reported {member} {text:?} is no instant: {error}"))
        .as_secs_f64()
}

/// Assert the published attribute `key` is the ISO-8601 spelling of the instant the reported
/// domain carries in `member`.
fn assert_published_instant(
    case: &str,
    described: &str,
    key: &str,
    domain: &Value,
    member: &str,
) {
    let Some(published) = output_value(described, key) else {
        return;
    };
    let parsed = aws_smithy_types::DateTime::from_str(
        &published,
        aws_smithy_types::date_time::Format::DateTime,
    )
    .unwrap_or_else(|error| {
        panic!(
            "case {case:?}: expected the published {key} {published:?} to parse as an ISO-8601 \
             instant, got {error}"
        )
    });
    let reported = reported_instant(domain, member);
    let drift = microseconds_of(parsed.as_secs_f64()) - microseconds_of(reported);
    assert!(
        drift.abs() <= 1,
        "case {case:?}: expected the published {key} to be the instant the domain reports \
         ({reported}) to the microsecond, got {published:?} ({drift} microseconds apart) against \
         {domain}"
    );
}

#[tokio::test]
async fn a_deployed_stack_publishes_the_domain_customer_profiles_created() {
    for row in rows() {
        let router = router();
        let case = row.case;
        let described = deployed(&router, case, row.stack_name, &row.template).await;

        for declared in &row.declared {
            let name = output_value(&described, declared.name_output).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: no {} output in {described}",
                    declared.name_output
                )
            });
            match declared.declared_name {
                // A declaration that NAMES the domain decides the name, so the handle the
                // stack publishes is exactly it.
                Some(stated) => assert_eq!(
                    name, stated,
                    "case {case:?}: expected the declared name to be the published handle, got \
                     {name:?}"
                ),
                // One that does not leaves it to CloudFormation, whose generated name still has
                // to be inside the syntax the owning service publishes.
                None => assert!(
                    is_a_published_domain_name(&name),
                    "case {case:?}: expected a generated name inside the owning service's \
                     published syntax, got {name:?}"
                ),
            }

            // THE RULE: the domain the stack claims to have created is a domain Amazon Connect
            // Customer Profiles holds, under the name the stack published, configured as the
            // template declared.
            let (status, reported) = get_domain(&router, &name).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the owning service to report the domain the stack \
                 created under {name:?}, got {status} with {reported}"
            );
            assert_eq!(
                reported.get(NAME_PROPERTY).and_then(Value::as_str),
                Some(name.as_str()),
                "case {case:?}: expected the domain to carry the published name, got {reported}"
            );
            assert_eq!(
                reported
                    .get("DefaultExpirationDays")
                    .and_then(Value::as_i64),
                Some(declared.expiration_days),
                "case {case:?}: expected the declared expiration window, got {reported}"
            );
            assert_eq!(
                reported.get("DefaultEncryptionKey").and_then(Value::as_str),
                declared.encryption_key,
                "case {case:?}: expected the declared encryption key, got {reported}"
            );
            assert_eq!(
                reported.get("DeadLetterQueueUrl").and_then(Value::as_str),
                declared.dead_letter_queue_url,
                "case {case:?}: expected the declared dead-letter queue, got {reported}"
            );

            // ...and the values the type publishes are that service's own rather than
            // stand-ins: the instants it recorded and the counts it keeps over a domain that
            // holds nothing yet.
            assert_published_instant(case, &described, "CreatedAt", &reported, "CreatedAt");
            assert_published_instant(
                case,
                &described,
                "LastUpdatedAt",
                &reported,
                "LastUpdatedAt",
            );
            for (key, member) in [
                ("ProfileCount", "ProfileCount"),
                ("ObjectCount", "ObjectCount"),
                ("TotalSize", "TotalSize"),
                ("MeteringProfileCount", "MeteringProfileCount"),
            ] {
                let Some(published) = output_value(&described, key) else {
                    continue;
                };
                let counted = reported
                    .get("Stats")
                    .and_then(|stats| stats.get(member))
                    .and_then(Value::as_i64)
                    .unwrap_or_else(|| panic!("case {case:?}: no Stats.{member} in {reported}"));
                assert_eq!(
                    published,
                    counted.to_string(),
                    "case {case:?}: expected the published Stats.{member} to be the count the \
                     domain reports ({counted}), got {published:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn the_deployed_domain_serves_its_own_data_plane() {
    // A domain a stack reports as created is only a domain if the calls a domain serves reach
    // it: a profile created in it, found again by a search over it, and counted in the
    // statistics that service keeps for it.
    let router = router();
    let case = "a profile created and searched in the deployed domain";
    let stack_name = "customer-profiles-data-plane";
    let template = json!({
        "Resources": {
            "Profiles": {
                "Type": DOMAIN,
                "Properties": {
                    NAME_PROPERTY: "cfn-profiles-data-plane",
                    "DefaultExpirationDays": 30,
                }
            }
        },
        "Outputs": {"DomainName": {"Value": {"Ref": "Profiles"}}}
    })
    .to_string();

    let described = deployed(&router, case, stack_name, &template).await;
    let name = output_value(&described, "DomainName")
        .unwrap_or_else(|| panic!("case {case:?}: no DomainName output in {described}"));

    let (status, created) = customer_profiles(
        &router,
        "POST",
        &format!("/domains/{name}/profiles"),
        Some(&json!({
            "FirstName": "Ada",
            "LastName": "Lovelace",
            "EmailAddress": "ada@example.com",
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateProfile to succeed against the deployed domain, got \
         {status} with {created}"
    );
    let profile_id = created
        .get("ProfileId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no ProfileId in {created}"))
        .to_string();

    // `_email` is one of the predefined search keys `SearchProfiles$KeyName` publishes, and the
    // profile's own email member populates it.
    let (status, found) = customer_profiles(
        &router,
        "POST",
        &format!("/domains/{name}/profiles/search"),
        Some(&json!({"KeyName": "_email", "Values": ["ada@example.com"]})),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected SearchProfiles to succeed against the deployed domain, got \
         {status} with {found}"
    );
    let ids: Vec<&str> = found
        .get("Items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("ProfileId").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        ids,
        vec![profile_id.as_str()],
        "case {case:?}: expected the search to find the profile created in the deployed domain, \
         got {found}"
    );

    // ...and the statistics that service keeps for the domain count it.
    let (status, reported) = get_domain(&router, &name).await;
    let counted = reported
        .get("Stats")
        .and_then(|stats| stats.get("ProfileCount"))
        .and_then(Value::as_i64);
    assert_eq!(
        (status, counted),
        (200, Some(1)),
        "case {case:?}: expected the domain to count the one profile it now holds, got {status} \
         with {reported}"
    );
}

#[tokio::test]
async fn a_changed_declaration_is_applied_to_the_domain_the_stack_has() {
    // `UpdateDomain` reconfigures a domain that exists, so a deployment that states another
    // expiration window is applied to the domain the stack already has — under the same name —
    // rather than being quietly dropped.
    let router = router();
    let case = "an update that widens the expiration window";
    let stack_name = "customer-profiles-updated";
    let domain_name = "cfn-profiles-updated";
    let template_of = |days: i64| {
        json!({
            "Resources": {
                "Profiles": {
                    "Type": DOMAIN,
                    "Properties": {
                        NAME_PROPERTY: domain_name,
                        "DefaultExpirationDays": days,
                    }
                }
            },
            "Outputs": {"DomainName": {"Value": {"Ref": "Profiles"}}}
        })
        .to_string()
    };

    deployed(&router, case, stack_name, &template_of(30)).await;

    let (status, updated) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack_name),
            member("TemplateBody", &template_of(MAX_EXPIRATION_DAYS)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {updated}"
    );
    let reached = settled_stack_status(case, &router, REGION, stack_name).await;
    assert_eq!(
        reached, "UPDATE_COMPLETE",
        "case {case:?}: expected the update to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(&router, REGION, stack_name).await
    );

    let (status, reported) = get_domain(&router, domain_name).await;
    assert_eq!(
        (
            status,
            reported
                .get("DefaultExpirationDays")
                .and_then(Value::as_i64)
        ),
        (200, Some(MAX_EXPIRATION_DAYS)),
        "case {case:?}: expected the window the update declared to reach the domain, got \
         {status} with {reported}"
    );
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_domains_it_created() {
    let router = router();
    let case = "two domains deleted with their stack";
    let stack_name = "customer-profiles-deleted";
    let template = json!({
        "Resources": {
            "First": {
                "Type": DOMAIN,
                "Properties": {
                    NAME_PROPERTY: "cfn-profiles-deleted-first",
                    "DefaultExpirationDays": 30,
                }
            },
            "Second": {
                "Type": DOMAIN,
                "Properties": { "DefaultExpirationDays": 60 }
            }
        },
        "Outputs": {
            "FirstName": {"Value": {"Ref": "First"}},
            "SecondName": {"Value": {"Ref": "Second"}}
        }
    })
    .to_string();

    let described = deployed(&router, case, stack_name, &template).await;
    let names: Vec<String> = ["FirstName", "SecondName"]
        .iter()
        .map(|key| {
            output_value(&described, key)
                .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"))
        })
        .collect();
    for name in &names {
        let (status, reported) = get_domain(&router, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the domain {name:?} to exist before the delete, got \
             {status} with {reported}"
        );
    }

    let (status, deleted) = call(&router, "DeleteStack", &[member("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
    );
    settle_the_deletion(&router, case, &names).await;

    // The domains the stack created are gone from the owning service, which is what makes a
    // stack's resources the stack's own rather than leftovers.
    for name in &names {
        let (status, reported) = get_domain(&router, name).await;
        assert_eq!(
            (status, reported.get("__type").and_then(Value::as_str)),
            (NOT_FOUND_STATUS, Some(RESOURCE_NOT_FOUND)),
            "case {case:?}: expected the domain {name:?} to be gone with the stack, got {status} \
             with {reported}"
        );
    }
}

#[tokio::test]
async fn a_parameterised_declaration_under_a_tagged_stack_reaches_the_domain() {
    // The deploy shape an infrastructure-as-code client submits: the domain's name comes from a
    // template PARAMETER rather than being written into the body, and the stack carries its own
    // `Tags`, which CloudFormation propagates to the resources it creates. The declaration has
    // to reach the owning service all the same.
    let router = router();
    let case = "a parameterised domain name under a stack carrying its own tags";
    let stack_name = "customer-profiles-parameterised";
    let domain_name = "cfn-profiles-parameterised";
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Parameters": {"ProfilesDomainName": {"Type": "String"}},
        "Resources": {
            "Profiles": {
                "Type": DOMAIN,
                "Properties": {
                    NAME_PROPERTY: {"Ref": "ProfilesDomainName"},
                    "DefaultExpirationDays": 30,
                }
            }
        },
        "Outputs": {"DomainName": {"Value": {"Ref": "Profiles"}}}
    })
    .to_string();

    let (status, body) = create_stack(
        &router,
        stack_name,
        &template,
        &[
            member("Parameters.member.1.ParameterKey", "ProfilesDomainName"),
            member("Parameters.member.1.ParameterValue", domain_name),
            member("Tags.member.1.Key", "sim2real"),
            member("Tags.member.1.Value", "customer-profiles-lifecycle"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let reached = settled_stack_status(case, &router, REGION, stack_name).await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(&router, REGION, stack_name).await
    );

    let (status, reported) = get_domain(&router, domain_name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the owning service to report the domain the stack created, got \
         {status} with {reported}"
    );
    // The stack's own tag reaches the domain, which is what CloudFormation propagates to every
    // resource it creates.
    assert_eq!(
        reported
            .get("Tags")
            .and_then(|tags| tags.get("sim2real"))
            .and_then(Value::as_str),
        Some("customer-profiles-lifecycle"),
        "case {case:?}: expected the stack's own tag on the domain, got {reported}"
    );
}

#[tokio::test]
async fn a_declared_value_reaches_the_domain_in_either_spelling_a_template_gives_it() {
    // A template states a value either in JSON's own spelling or as the TEXT a parameter
    // reference resolves to — `Ref` on a parameter answers that parameter's value, which a
    // stack operation carries as a string whatever the parameter's declared type — so both
    // spellings have to reach the owning service as the kind its member is carried in.
    //
    // (case, the stack, the expiration window the declaration states, the flag the matching
    // configuration states, and the window the domain must report)
    let rows: [(&str, &str, Value, Value, i64); 3] = [
        (
            "the window as a JSON number and the flag as a JSON boolean",
            "customer-profiles-typed-json",
            json!(45),
            json!(true),
            45,
        ),
        (
            "both as the text a parameter resolves to",
            "customer-profiles-typed-text",
            json!("45"),
            json!("true"),
            45,
        ),
        (
            "the published maximum window as text, with the flag spelled in mixed case",
            "customer-profiles-typed-bound",
            json!(MAX_EXPIRATION_DAYS.to_string()),
            json!("False"),
            MAX_EXPIRATION_DAYS,
        ),
    ];

    for (case, stack_name, days, enabled, expected) in rows {
        let router = router();
        let domain_name = format!("cfn-profiles-{}", stack_name.len());
        let template = json!({
            "Resources": {
                "Profiles": {
                    "Type": DOMAIN,
                    "Properties": {
                        NAME_PROPERTY: domain_name,
                        "DefaultExpirationDays": days,
                        "Matching": {"Enabled": enabled},
                    }
                }
            },
            "Outputs": {"DomainName": {"Value": {"Ref": "Profiles"}}}
        })
        .to_string();

        deployed(&router, case, stack_name, &template).await;
        let (status, reported) = get_domain(&router, &domain_name).await;
        assert_eq!(
            (
                status,
                reported
                    .get("DefaultExpirationDays")
                    .and_then(Value::as_i64)
            ),
            (200, Some(expected)),
            "case {case:?}: expected the declared window to reach the domain, got {status} with \
             {reported}"
        );
        assert!(
            reported
                .get("Matching")
                .and_then(|matching| matching.get("Enabled"))
                .is_some_and(Value::is_boolean),
            "case {case:?}: expected the matching flag to reach the domain as a boolean, got \
             {reported}"
        );
    }
}

/// Wait until every domain of `names` is gone from the owning service.
///
/// Waits for the CONDITION rather than for a span, under the harness's own generous deadline,
/// so a loaded host cannot decide the outcome.
async fn settle_the_deletion(router: &MultiServiceRouter, case: &str, names: &[String]) {
    let deadline = std::time::Instant::now() + crate::harness::SETTLE_DEADLINE;
    loop {
        let mut still_held: BTreeMap<String, u16> = BTreeMap::new();
        for name in names {
            let (status, _) = get_domain(router, name).await;
            if status == 200 {
                still_held.insert(name.clone(), status);
            }
        }
        if still_held.is_empty() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected every domain the stack created to be released, these are \
             still held: {still_held:?}"
        );
        tokio::time::sleep(crate::harness::SETTLE_POLL_INTERVAL).await;
    }
}
