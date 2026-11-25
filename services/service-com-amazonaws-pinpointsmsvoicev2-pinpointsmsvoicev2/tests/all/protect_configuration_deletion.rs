// SPDX-License-Identifier: CC0-1.0

//! Deleting a protect configuration, end to end over the wire.
//!
//! Every request goes through [`MultiServiceRouter::dispatch`] as `awsJson1_0` — `POST /`
//! with `X-Amz-Target: PinpointSMSVoiceV2.<Op>` — so these tests exercise routing,
//! deserialization, constraint validation, the handler and serialization, exactly as a
//! real SDK client would reach them.
//!
//! The rule under test: a delete of a protect configuration removes the stored record and
//! reports it, but only while the documented precondition holds — `DeleteProtectConfiguration`
//! documents that "the protect configuration must have deletion protection disabled and
//! must not be associated as the account default protect configuration or associated with a
//! configuration set" — and each state that breaks it is refused as `ConflictException`
//! carrying the reason that names it, leaving the record exactly as it was; a request that
//! names no stored configuration is `ResourceNotFoundException`.
//!
//! Its harness is this suite's own: the sibling suites' helpers are private to their
//! modules, and a test target shares nothing it does not export.

use std::sync::Arc;

use service_com_amazonaws_pinpointsmsvoicev2_pinpointsmsvoicev2 as sms_voice;
use smithy_server_runtime::MultiServiceRouter;

/// The region these tests sign for, so the ARNs under assertion are not the default one.
const REGION: &str = "eu-west-1";

/// The `ResourceType` this resource's refusals carry, as the model's `ResourceType` enum
/// spells it (`smithy_query(query="shape",shape="ResourceType")`).
const RESOURCE_TYPE: &str = "protect-configuration";

/// One emulator: the shared engine plus a router with only this service mounted.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These tests assert behaviour, not signatures; the emulator verifies signatures
        // itself and a service never checks them.
        router.set_request_authentication(false);
        router.register_service(sms_voice::EmulatedService::new(engine));
        Self { router }
    }

    /// Call one operation and answer `(http status, parsed JSON body)`.
    async fn call(&self, operation: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("PinpointSMSVoiceV2.{operation}"))
            // The SigV4 credential scope is what routes the request to this service and
            // what names the region its ARNs carry.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/sms-voice/aws4_request, \
                     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(&body).expect("the request body serializes"),
            ))
            .expect("a valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(serde_json::Value::Null)
        };
        (status, parsed)
    }

    /// Call one operation and require HTTP 200, reporting the body when it is not.
    async fn ok(&self, operation: &str, body: serde_json::Value) -> serde_json::Value {
        let (status, parsed) = self.call(operation, body).await;
        assert_eq!(
            status, 200,
            "{operation}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }

    /// Create one protect configuration and answer its whole create response.
    async fn create_protect_configuration(
        &self,
        deletion_protection_enabled: bool,
        tags: &[(&str, &str)],
    ) -> serde_json::Value {
        let tags: Vec<serde_json::Value> = tags
            .iter()
            .map(|(key, value)| serde_json::json!({"Key": key, "Value": value}))
            .collect();
        self.ok(
            "CreateProtectConfiguration",
            serde_json::json!({
                "DeletionProtectionEnabled": deletion_protection_enabled,
                "Tags": tags,
            }),
        )
        .await
    }

    /// Create one configuration set and answer its name.
    async fn create_configuration_set(&self, name: &str) -> String {
        let created = self
            .ok(
                "CreateConfigurationSet",
                serde_json::json!({"ConfigurationSetName": name}),
            )
            .await;
        member(&created, "ConfigurationSetName")
    }

    /// Every protect configuration the account holds, by id, as a describe of the whole
    /// account reports them.
    async fn all_ids(&self) -> Vec<String> {
        let described = self
            .ok("DescribeProtectConfigurations", serde_json::json!({}))
            .await;
        let mut ids: Vec<String> = described
            .get("ProtectConfigurations")
            .and_then(serde_json::Value::as_array)
            .map(|records| {
                records
                    .iter()
                    .map(|record| member(record, "ProtectConfigurationId"))
                    .collect()
            })
            .unwrap_or_default();
        ids.sort();
        ids
    }
}

/// The wire error code of a failed call (`__type`, with any namespace prefix dropped).
fn error_code(body: &serde_json::Value) -> String {
    let raw = body
        .get("__type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<no __type>");
    raw.rsplit('#').next().unwrap_or(raw).to_string()
}

fn member(body: &serde_json::Value, name: &str) -> String {
    body.get(name)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("expected a string member {name} in {body}"))
        .to_string()
}

/// This service's conflict sentence: a restatement of the exception's own structured
/// members, as every other conflict this service raises words it.
fn conflict_sentence(reason: &str, resource_id: &str) -> String {
    format!(
        "Conflict Occurred - Reason=\"{reason}\" ResourceType=\"{RESOURCE_TYPE}\" \
         ResourceId=\"{resource_id}\""
    )
}

/// Assert one refusal: the status, the wire error code and the structured members.
fn assert_conflict(case: &str, status: u16, body: &serde_json::Value, reason: &str, id: &str) {
    assert_eq!(
        status, 400,
        "case {case}: expected HTTP 400, got {status} with {body}"
    );
    assert_eq!(
        error_code(body),
        "ConflictException",
        "case {case}: got {body}"
    );
    assert_eq!(member(body, "Reason"), reason, "case {case}: got {body}");
    assert_eq!(
        member(body, "ResourceType"),
        RESOURCE_TYPE,
        "case {case}: got {body}"
    );
    assert_eq!(member(body, "ResourceId"), id, "case {case}: got {body}");
    assert_eq!(
        member(body, "Message"),
        conflict_sentence(reason, id),
        "case {case}: got {body}"
    );
}

/// One row of [`a_delete_removes_the_configuration_and_reports_the_stored_record`].
struct DeleteCase {
    /// What this row is called in every assertion message.
    case: &'static str,
    /// Name the configuration by its ARN rather than by its id.
    by_arn: bool,
    /// The tags the configuration is created with.
    tags: &'static [(&'static str, &'static str)],
    /// How many other configurations the account holds while this one is deleted.
    others: usize,
}

/// A delete removes the record and reports it: the response is the configuration as it was
/// stored, the account no longer holds it, and every other configuration is untouched.
///
/// The rows differ along what the rule generalises over: whether the request names the id
/// or the ARN, whether the configuration carries tags, and how many other configurations
/// the account holds while it is deleted.
#[tokio::test]
async fn a_delete_removes_the_configuration_and_reports_the_stored_record() {
    let cases = &[
        DeleteCase {
            case: "id/untagged/alone",
            by_arn: false,
            tags: &[],
            others: 0,
        },
        DeleteCase {
            case: "arn/untagged/alone",
            by_arn: true,
            tags: &[],
            others: 0,
        },
        DeleteCase {
            case: "id/tagged/two others",
            by_arn: false,
            tags: &[("Name", "storefront")],
            others: 2,
        },
        DeleteCase {
            case: "arn/tagged/four others",
            by_arn: true,
            tags: &[("Name", "campaign"), ("Owner", "sms-team")],
            others: 4,
        },
    ];
    for DeleteCase {
        case,
        by_arn,
        tags,
        others,
    } in cases
    {
        let emulator = Emulator::new();
        let mut other_ids: Vec<String> = Vec::new();
        for _ in 0..*others {
            let created = emulator.create_protect_configuration(false, &[]).await;
            other_ids.push(member(&created, "ProtectConfigurationId"));
        }
        let created = emulator.create_protect_configuration(false, tags).await;
        let id = member(&created, "ProtectConfigurationId");
        let arn = member(&created, "ProtectConfigurationArn");
        let named = if *by_arn { arn.clone() } else { id.clone() };

        let deleted = emulator
            .ok(
                "DeleteProtectConfiguration",
                serde_json::json!({"ProtectConfigurationId": named}),
            )
            .await;
        assert_eq!(
            member(&deleted, "ProtectConfigurationId"),
            id,
            "case {case}: the stored id is reported, got {deleted}"
        );
        assert_eq!(
            member(&deleted, "ProtectConfigurationArn"),
            arn,
            "case {case}: the stored ARN is reported, got {deleted}"
        );
        assert_eq!(
            deleted.get("CreatedTimestamp"),
            created.get("CreatedTimestamp"),
            "case {case}: the creation instant of the removed record is reported, got {deleted}"
        );
        assert_eq!(
            deleted.get("AccountDefault"),
            Some(&serde_json::json!(false)),
            "case {case}: a configuration nothing made the account default is not it, got {deleted}"
        );
        assert_eq!(
            deleted.get("DeletionProtectionEnabled"),
            Some(&serde_json::json!(false)),
            "case {case}: the stored deletion-protection flag is reported, got {deleted}"
        );

        // The resource is gone: it is in no describe of the account, a describe that names
        // it is a not-found, and so is a second delete.
        other_ids.sort();
        assert_eq!(
            emulator.all_ids().await,
            other_ids,
            "case {case}: the deleted configuration is gone and the others are untouched"
        );
        for operation in ["DescribeProtectConfigurations", "DeleteProtectConfiguration"] {
            let body = if operation == "DescribeProtectConfigurations" {
                serde_json::json!({"ProtectConfigurationIds": [id.clone()]})
            } else {
                serde_json::json!({"ProtectConfigurationId": id.clone()})
            };
            let (status, answered) = emulator.call(operation, body).await;
            assert_eq!(
                status, 400,
                "case {case}/{operation}: expected HTTP 400, got {status} with {answered}"
            );
            assert_eq!(
                error_code(&answered),
                "ResourceNotFoundException",
                "case {case}/{operation}: got {answered}"
            );
        }
    }
}

/// A configuration whose deletion protection is enabled is refused, and the refusal writes
/// nothing: the configuration is still there afterwards.
///
/// The rows differ along the form the request names the resource in; the refusal reports
/// the STORED id either way.
#[tokio::test]
async fn deletion_protection_refuses_the_delete_and_keeps_the_configuration() {
    let emulator = Emulator::new();
    let created = emulator.create_protect_configuration(true, &[]).await;
    let id = member(&created, "ProtectConfigurationId");
    let arn = member(&created, "ProtectConfigurationArn");
    assert_eq!(
        created.get("DeletionProtectionEnabled"),
        Some(&serde_json::json!(true)),
        "the create stored the deletion-protection flag it was given, got {created}"
    );
    for named in [&id, &arn] {
        let (status, body) = emulator
            .call(
                "DeleteProtectConfiguration",
                serde_json::json!({"ProtectConfigurationId": named}),
            )
            .await;
        assert_conflict(
            &format!("named by {named}"),
            status,
            &body,
            "DELETION_PROTECTION_ENABLED",
            &id,
        );
        assert_eq!(
            emulator.all_ids().await,
            vec![id.clone()],
            "case {named}: a refused delete leaves the configuration in place"
        );
    }
}

/// A configuration associated with a configuration set is refused while ANY set holds the
/// association, and is deletable once no set does — whichever way the association went
/// away.
///
/// The rows differ along how the link is cleared and how many sets hold it: an explicit
/// disassociate, and deleting the set that holds it (deleting a set takes its association
/// with it, which is what makes the configuration deletable afterwards).
#[tokio::test]
async fn an_association_with_a_configuration_set_refuses_the_delete_until_it_is_gone() {
    // (case name, how many sets associate the configuration, clear the link by deleting
    // the set rather than disassociating).
    let cases: &[(&str, usize, bool)] = &[
        ("one set/disassociate", 1, false),
        ("one set/delete the set", 1, true),
        ("three sets/disassociate", 3, false),
        ("three sets/delete the sets", 3, true),
    ];
    for (case, set_count, clear_by_deleting_the_set) in cases.iter() {
        let emulator = Emulator::new();
        let created = emulator.create_protect_configuration(false, &[]).await;
        let id = member(&created, "ProtectConfigurationId");
        let mut set_names: Vec<String> = Vec::new();
        for index in 0..*set_count {
            let name = emulator
                .create_configuration_set(&format!("set-{index}"))
                .await;
            emulator
                .ok(
                    "AssociateProtectConfiguration",
                    serde_json::json!({
                        "ProtectConfigurationId": id,
                        "ConfigurationSetName": name,
                    }),
                )
                .await;
            set_names.push(name);
        }

        // Every set but the last is released first, so the refusal below is about the ONE
        // association still recorded rather than about all of them.
        for (cleared, name) in set_names.iter().enumerate() {
            let (status, body) = emulator
                .call(
                    "DeleteProtectConfiguration",
                    serde_json::json!({"ProtectConfigurationId": id}),
                )
                .await;
            assert_conflict(
                &format!("{case}: {} association(s) left", set_names.len() - cleared),
                status,
                &body,
                "PROTECT_CONFIGURATION_ASSOCIATED_WITH_CONFIGURATION_SET",
                &id,
            );
            assert_eq!(
                emulator.all_ids().await,
                vec![id.clone()],
                "case {case}: a refused delete leaves the configuration in place"
            );
            if *clear_by_deleting_the_set {
                emulator
                    .ok(
                        "DeleteConfigurationSet",
                        serde_json::json!({"ConfigurationSetName": name}),
                    )
                    .await;
            } else {
                emulator
                    .ok(
                        "DisassociateProtectConfiguration",
                        serde_json::json!({
                            "ProtectConfigurationId": id,
                            "ConfigurationSetName": name,
                        }),
                    )
                    .await;
            }
        }

        // No set holds the association any more, so the precondition holds and the delete
        // is served.
        let deleted = emulator
            .ok(
                "DeleteProtectConfiguration",
                serde_json::json!({"ProtectConfigurationId": id}),
            )
            .await;
        assert_eq!(
            member(&deleted, "ProtectConfigurationId"),
            id,
            "case {case}: the delete reports the configuration it removed, got {deleted}"
        );
        assert!(
            emulator.all_ids().await.is_empty(),
            "case {case}: the configuration is gone, got {:?}",
            emulator.all_ids().await
        );
    }
}

/// Deleting one configuration leaves another set's association, and every other
/// configuration, exactly as it was: the refusal is about the configuration the request
/// named, not about the account.
#[tokio::test]
async fn an_association_refuses_only_the_configuration_it_names() {
    let emulator = Emulator::new();
    let associated = member(
        &emulator.create_protect_configuration(false, &[]).await,
        "ProtectConfigurationId",
    );
    let free = member(
        &emulator.create_protect_configuration(false, &[]).await,
        "ProtectConfigurationId",
    );
    let set = emulator.create_configuration_set("shared-set").await;
    emulator
        .ok(
            "AssociateProtectConfiguration",
            serde_json::json!({
                "ProtectConfigurationId": associated,
                "ConfigurationSetName": set,
            }),
        )
        .await;

    let (status, body) = emulator
        .call(
            "DeleteProtectConfiguration",
            serde_json::json!({"ProtectConfigurationId": associated}),
        )
        .await;
    assert_conflict(
        "the associated configuration",
        status,
        &body,
        "PROTECT_CONFIGURATION_ASSOCIATED_WITH_CONFIGURATION_SET",
        &associated,
    );

    // The other configuration is associated with nothing, so its delete is served.
    emulator
        .ok(
            "DeleteProtectConfiguration",
            serde_json::json!({"ProtectConfigurationId": free}),
        )
        .await;
    assert_eq!(
        emulator.all_ids().await,
        vec![associated.clone()],
        "only the configuration whose precondition held was removed"
    );
}

/// A request that names no stored configuration is refused as `ResourceNotFoundException`,
/// whichever form it names it in and whether the id was never minted or was deleted.
#[tokio::test]
async fn an_unknown_configuration_is_not_found() {
    let emulator = Emulator::new();
    // A created configuration, so the store is not empty and the refusal is about the id
    // the request named rather than about an empty store.
    let kept = member(
        &emulator.create_protect_configuration(false, &[]).await,
        "ProtectConfigurationId",
    );
    let removed = emulator.create_protect_configuration(false, &[]).await;
    let removed_id = member(&removed, "ProtectConfigurationId");
    let removed_arn = member(&removed, "ProtectConfigurationArn");
    emulator
        .ok(
            "DeleteProtectConfiguration",
            serde_json::json!({"ProtectConfigurationId": removed_id}),
        )
        .await;

    let named = [
        // Never minted: within `ProtectConfigurationIdOrArn` but naming nothing.
        "protect-configuration-ffffffffffffffffffffffffffffffff".to_string(),
        "arn:aws:sms-voice:eu-west-1:000000000000:protect-configuration/protect-configuration-0000000000000000000000000000beef".to_string(),
        // Minted and then deleted, in both forms.
        removed_id.clone(),
        removed_arn.clone(),
    ];
    for name in &named {
        let (status, body) = emulator
            .call(
                "DeleteProtectConfiguration",
                serde_json::json!({"ProtectConfigurationId": name}),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {name}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {name}: got {body}"
        );
        assert_eq!(
            member(&body, "ResourceType"),
            RESOURCE_TYPE,
            "case {name}: got {body}"
        );
        let reported = member(&body, "ResourceId");
        assert!(
            name.ends_with(&reported),
            "case {name}: the refusal names the configuration the request named, got {reported}"
        );
        assert_eq!(
            member(&body, "Message"),
            format!(
                "Resource Not Found - ResourceType=\"{RESOURCE_TYPE}\" ResourceId=\"{reported}\""
            ),
            "case {name}: got {body}"
        );
    }

    // The configuration that was never deleted is still there: a refused delete wrote
    // nothing.
    assert_eq!(
        emulator.all_ids().await,
        vec![kept],
        "a not-found delete leaves the account's other configurations in place"
    );
}
