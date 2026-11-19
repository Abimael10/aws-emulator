// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `UpdateSpace`'s PARTIAL-update rule.
//!
//! `UpdateSpace` is documented as "Only the provided fields are changed; omitted fields
//! are left unchanged", so the suite asserts that rule over a TABLE rather than one
//! recorded exchange: each row starts from a space in a different shape (with and without
//! an AgentCore role, with and without a customer managed key), patches a different
//! subset of the three updatable members, and requires
//!
//! * every member the request carried to hold its new value;
//! * every member the request omitted — including the ones the input shape does not even
//!   carry (the id, ARN, parent, region, owner, data-access role, status, `createdAt`) —
//!   to hold exactly what it held before;
//! * `encryptionStrategy` AWS_OWNED to drop the customer managed key, per the member's
//!   own documentation;
//! * the patched record, not a second copy of it, to be what `GetSpace` and `ListSpaces`
//!   report afterwards.
//!
//! The refusals are asserted the same way: a space the store does not hold is the modelled
//! 404, an inconsistent `encryptionConfiguration` is the modelled 400, and neither writes
//! anything. Each assertion names its case and the actual value.

use std::sync::Arc;

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerializableStruct, ShapeSerializer};
use aws_smithy_schema::Schema;
use aws_smithy_schema_server::{CborCodec, DeserializableStruct};
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError, Service,
};

use service_com_amazonaws_cloudwatchomni_cloudwatchomnifrontend as omni;

use omni::ops::create_domain::CreateDomain;
use omni::ops::create_space::CreateSpace;
use omni::ops::get_space::{GetSpace, GetSpaceError};
use omni::ops::list_spaces::ListSpaces;
use omni::ops::update_space::{UpdateSpace, UpdateSpaceError};
use omni::ops::types::{
    CreateDomainInput, CreateSpaceInput, EncryptionConfiguration, GetSpaceInput, GetSpaceOutput,
    ListSpacesInput, Space, UpdateSpaceInput, UpdateSpaceOutput, UPDATE_SPACE_INPUT_SCHEMA,
};

/// The region the suite signs for.
const REGION: &str = "us-west-2";

/// A role ARN satisfying `Arn`'s `^arn:.+$`.
const DATA_ACCESS_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/OmniData";

/// An AgentCore evaluation role ARN satisfying `Arn`'s `^arn:.+$`.
const AGENT_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/OmniAgentCore";

/// A second AgentCore evaluation role ARN, so a row can change the role it holds.
const OTHER_AGENT_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/OmniAgentCoreNext";

/// A KMS key ARN satisfying `KmsKeyArn`'s modelled
/// `^arn:aws[a-zA-Z-]*:kms:[a-z0-9-]+:[0-9]{12}:key/(mrk-)?[a-f0-9-]+$`.
const CUSTOMER_KEY_ARN: &str =
    "arn:aws:kms:us-west-2:000000000000:key/8a4f1b3c-0e2d-4a5b-9c7d-1f2e3a4b5c6d";

/// A second KMS key ARN, so a row can rotate the key a space uses.
const OTHER_CUSTOMER_KEY_ARN: &str =
    "arn:aws:kms:us-west-2:000000000000:key/mrk-1d2c3b4a5e6f7a8b9c0d1e2f3a4b5c6d";

/// `EncryptionStrategy`'s two modelled values.
const AWS_OWNED: &str = "AWS_OWNED";
const CUSTOMER_MANAGED: &str = "CUSTOMER_MANAGED";

/// A well-formed `SpaceId` that names nothing.
const MISSING_SPACE_ID: &str = "00000000-0000-4000-8000-000000000000";

/// How an encryption configuration is written in a row: the strategy and its optional key.
type Encryption = (&'static str, Option<&'static str>);

fn encryption(configuration: Encryption) -> EncryptionConfiguration {
    EncryptionConfiguration {
        encryption_strategy: configuration.0.to_string(),
        kms_key_arn: configuration.1.map(str::to_string),
    }
}

/// A SigV4 `Authorization` header whose credential scope names this service's signing
/// name and the region the handlers record.
fn signed_request_parts() -> http::request::Parts {
    let (parts, ()) = http::Request::builder()
        .method("POST")
        .uri("/service/CloudWatchOmniFrontend/operation/Unused")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/cloudwatch/aws4_request, \
                 SignedHeaders=host, Signature=emulator"
            ),
        )
        .body(())
        .expect("a well-formed request")
        .into_parts();
    parts
}

/// One emulator: one shared engine, the service mounted on a router, and the request
/// metadata a handler reads.
struct Omni {
    engine: Arc<engine::InMemoryEngine>,
    service: omni::EmulatedService<engine::InMemoryEngine>,
    router: MultiServiceRouter,
    parts: http::request::Parts,
}

impl Omni {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(omni::EmulatedService::new(Arc::clone(&engine)));
        Self {
            service: omni::EmulatedService::new(Arc::clone(&engine)),
            engine,
            router,
            parts: signed_request_parts(),
        }
    }

    fn ctx(&self) -> OperationContext<'_> {
        OperationContext::new(
            &self.parts,
            self.service.definition().identity(),
            EmbeddedEmulator::from_router(self.router.clone()),
        )
    }

    /// A domain to hang the suite's spaces off.
    ///
    /// The create carries the `clientToken` it is admitted only with, derived from the
    /// domain's own name.
    async fn domain(&self, name: &str) -> String {
        CreateDomain::new(Arc::clone(&self.engine))
            .handle(
                CreateDomainInput {
                    name: name.to_string(),
                    identity_providers: vec!["IAM".to_string()],
                    client_token: Some(format!("token-{name}")),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("CreateDomain({name:?}) failed: {error:?}"))
            .domain
            .domain_id
    }

    async fn create_space(
        &self,
        case: &str,
        domain_id: &str,
        name: &str,
        agent_role: Option<&str>,
        configuration: Option<Encryption>,
    ) -> Space {
        CreateSpace::new(Arc::clone(&self.engine))
            .handle(
                CreateSpaceInput {
                    name: name.to_string(),
                    domain_id: domain_id.to_string(),
                    data_access_role_arn: DATA_ACCESS_ROLE_ARN.to_string(),
                    agent_core_evaluation_role_arn: agent_role.map(str::to_string),
                    encryption_configuration: configuration.map(encryption),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateSpace failed: {error:?}"))
            .space
    }

    async fn update_space(
        &self,
        input: &UpdateSpaceInput,
    ) -> Result<UpdateSpaceOutput, UpdateSpaceError> {
        UpdateSpace::new(Arc::clone(&self.engine))
            .handle(input.clone(), &self.ctx())
            .await
    }

    async fn get_space(&self, space_id: &str) -> Result<GetSpaceOutput, GetSpaceError> {
        GetSpace::new(Arc::clone(&self.engine))
            .handle(
                GetSpaceInput {
                    space_id: space_id.to_string(),
                },
                &self.ctx(),
            )
            .await
    }

    /// The name `ListSpaces` reports for `space_id`, so the suite can check the summary
    /// projection follows the patched record.
    async fn listed_name(&self, case: &str, space_id: &str) -> String {
        let listed = ListSpaces::new(Arc::clone(&self.engine))
            .handle(
                ListSpacesInput {
                    domain_id: None,
                    next_token: None,
                    max_results: None,
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: ListSpaces failed: {error:?}"));
        listed
            .items
            .into_iter()
            .find(|summary| summary.space_id == space_id)
            .map(|summary| summary.name)
            .unwrap_or_else(|| panic!("case {case:?}: ListSpaces does not report {space_id:?}"))
    }

    /// Dispatch one rpcv2Cbor request over HTTP and return the status and body.
    async fn over_the_wire<I: SerializableStruct>(
        &self,
        operation: &str,
        input_schema: &Schema,
        input: &I,
    ) -> (u16, bytes::Bytes) {
        let codec = CborCodec::default();
        let mut serializer = codec.create_serializer();
        serializer
            .write_struct(input_schema, input)
            .expect("the input serializes");
        let mut builder = http::Request::builder()
            .method("POST")
            .uri(format!(
                "/service/CloudWatchOmniFrontend/operation/{operation}"
            ))
            .header("content-type", "application/cbor")
            .header("smithy-protocol", "rpc-v2-cbor");
        for (name, value) in &self.parts.headers {
            builder = builder.header(name, value);
        }
        let request = builder
            .body(bytes::Bytes::from(serializer.finish()))
            .expect("a well-formed rpcv2Cbor request");
        let response = self.router.dispatch(request).await;
        (response.status().as_u16(), response.body().clone())
    }
}

/// Require a modelled failure with the status and error code the model declares for it.
fn expect_modelled_error<O: std::fmt::Debug, E: OperationError + std::fmt::Debug>(
    case: &str,
    result: Result<O, E>,
    expected_status: u16,
    expected_code: &str,
) {
    match result {
        Ok(output) => {
            panic!("case {case:?}: expected {expected_code} ({expected_status}), got Ok({output:?})")
        }
        Err(error) => {
            assert_eq!(
                error.shape_name(),
                expected_code,
                "case {case:?}: expected error code {expected_code}, got {error:?}"
            );
            assert_eq!(
                error.http_status(),
                expected_status,
                "case {case:?}: expected HTTP {expected_status} for {expected_code}, got {}",
                error.http_status()
            );
        }
    }
}

/// The members of a space that `UpdateSpace`'s input cannot name, rendered for comparison:
/// a patch must bring every one of them through untouched.
fn untouched(space: &Space) -> String {
    format!(
        "{:?}",
        (
            &space.space_id,
            &space.space_arn,
            &space.domain_arn,
            &space.region,
            &space.owner_account_id,
            &space.data_access_role_arn,
            &space.status,
            &space.status_reason,
            space.created_at.secs(),
            space.created_at.subsec_nanos(),
        )
    )
}

/// The encryption a space reports, as the row writes it.
fn reported_encryption(case: &str, space: &Space) -> (String, Option<String>) {
    // `Space.encryptionConfiguration` is "Always populated".
    let configuration = space
        .encryption_configuration
        .clone()
        .unwrap_or_else(|| panic!("case {case:?}: encryptionConfiguration is absent"));
    (configuration.encryption_strategy, configuration.kms_key_arn)
}

/// One row of the partial-update table.
struct Patch {
    case: &'static str,
    /// The space to start from.
    initial_name: String,
    initial_role: Option<&'static str>,
    initial_encryption: Option<Encryption>,
    /// The members the `UpdateSpace` request carries.
    patch_name: Option<String>,
    patch_role: Option<&'static str>,
    patch_encryption: Option<Encryption>,
    /// What the space must report afterwards.
    expected_name: String,
    expected_role: Option<&'static str>,
    expected_encryption: Encryption,
}

fn table() -> Vec<Patch> {
    // Rows differ along WHICH members the patch carries (each one alone, all three at
    // once, and none at all), along the shape of the space they start from, and along
    // `name`'s size — "abc" and a 64-character name are both published bounds of its
    // modelled `@length(3..64)`.
    vec![
        Patch {
            case: "name only keeps the role and the customer managed key",
            initial_name: "space-one".to_string(),
            initial_role: Some(AGENT_ROLE_ARN),
            initial_encryption: Some((CUSTOMER_MANAGED, Some(CUSTOMER_KEY_ARN))),
            patch_name: Some("space-one-renamed".to_string()),
            patch_role: None,
            patch_encryption: None,
            expected_name: "space-one-renamed".to_string(),
            expected_role: Some(AGENT_ROLE_ARN),
            expected_encryption: (CUSTOMER_MANAGED, Some(CUSTOMER_KEY_ARN)),
        },
        Patch {
            case: "the role alone is added to a space created without one",
            initial_name: "space-two".to_string(),
            initial_role: None,
            initial_encryption: None,
            patch_name: None,
            patch_role: Some(AGENT_ROLE_ARN),
            patch_encryption: None,
            expected_name: "space-two".to_string(),
            expected_role: Some(AGENT_ROLE_ARN),
            // A space created with no encryption configuration reports AWS_OWNED.
            expected_encryption: (AWS_OWNED, None),
        },
        Patch {
            case: "the role alone is replaced",
            initial_name: "space-three".to_string(),
            initial_role: Some(AGENT_ROLE_ARN),
            initial_encryption: Some((AWS_OWNED, None)),
            patch_name: None,
            patch_role: Some(OTHER_AGENT_ROLE_ARN),
            patch_encryption: None,
            expected_name: "space-three".to_string(),
            expected_role: Some(OTHER_AGENT_ROLE_ARN),
            expected_encryption: (AWS_OWNED, None),
        },
        Patch {
            case: "a customer managed key alone is adopted",
            initial_name: "space-four".to_string(),
            initial_role: Some(AGENT_ROLE_ARN),
            initial_encryption: None,
            patch_name: None,
            patch_role: None,
            patch_encryption: Some((CUSTOMER_MANAGED, Some(CUSTOMER_KEY_ARN))),
            expected_name: "space-four".to_string(),
            expected_role: Some(AGENT_ROLE_ARN),
            expected_encryption: (CUSTOMER_MANAGED, Some(CUSTOMER_KEY_ARN)),
        },
        Patch {
            case: "a customer managed key alone is rotated",
            initial_name: "space-five".to_string(),
            initial_role: None,
            initial_encryption: Some((CUSTOMER_MANAGED, Some(CUSTOMER_KEY_ARN))),
            patch_name: None,
            patch_role: None,
            patch_encryption: Some((CUSTOMER_MANAGED, Some(OTHER_CUSTOMER_KEY_ARN))),
            expected_name: "space-five".to_string(),
            expected_role: None,
            expected_encryption: (CUSTOMER_MANAGED, Some(OTHER_CUSTOMER_KEY_ARN)),
        },
        Patch {
            // The `encryptionConfiguration` member doc: "Pass `encryptionStrategy`
            // AWS_OWNED to stop using a customer managed key and revert to service owned
            // encryption."
            case: "AWS_OWNED reverts a customer managed key to service owned encryption",
            initial_name: "space-six".to_string(),
            initial_role: Some(AGENT_ROLE_ARN),
            initial_encryption: Some((CUSTOMER_MANAGED, Some(CUSTOMER_KEY_ARN))),
            patch_name: None,
            patch_role: None,
            patch_encryption: Some((AWS_OWNED, None)),
            expected_name: "space-six".to_string(),
            expected_role: Some(AGENT_ROLE_ARN),
            expected_encryption: (AWS_OWNED, None),
        },
        Patch {
            case: "every updatable member at once, name at @length(min: 3)",
            initial_name: "space-seven".to_string(),
            initial_role: None,
            initial_encryption: None,
            patch_name: Some("abc".to_string()),
            patch_role: Some(OTHER_AGENT_ROLE_ARN),
            patch_encryption: Some((CUSTOMER_MANAGED, Some(CUSTOMER_KEY_ARN))),
            expected_name: "abc".to_string(),
            expected_role: Some(OTHER_AGENT_ROLE_ARN),
            expected_encryption: (CUSTOMER_MANAGED, Some(CUSTOMER_KEY_ARN)),
        },
        Patch {
            case: "a name at @length(max: 64)",
            initial_name: "space-eight".to_string(),
            initial_role: Some(AGENT_ROLE_ARN),
            initial_encryption: Some((CUSTOMER_MANAGED, Some(CUSTOMER_KEY_ARN))),
            patch_name: Some("n".repeat(64)),
            patch_role: None,
            patch_encryption: None,
            expected_name: "n".repeat(64),
            expected_role: Some(AGENT_ROLE_ARN),
            expected_encryption: (CUSTOMER_MANAGED, Some(CUSTOMER_KEY_ARN)),
        },
        Patch {
            case: "a request carrying no updatable member leaves the space whole",
            initial_name: "space-nine".to_string(),
            initial_role: Some(AGENT_ROLE_ARN),
            initial_encryption: Some((CUSTOMER_MANAGED, Some(CUSTOMER_KEY_ARN))),
            patch_name: None,
            patch_role: None,
            patch_encryption: None,
            expected_name: "space-nine".to_string(),
            expected_role: Some(AGENT_ROLE_ARN),
            expected_encryption: (CUSTOMER_MANAGED, Some(CUSTOMER_KEY_ARN)),
        },
    ]
}

#[tokio::test]
async fn update_space_changes_only_the_members_the_request_carries() {
    let omni = Omni::new();
    let parent = omni.domain("omni-update-space").await;

    for row in table() {
        let case = row.case;
        let created = omni
            .create_space(
                case,
                &parent,
                &row.initial_name,
                row.initial_role,
                row.initial_encryption,
            )
            .await;

        let updated = omni
            .update_space(&UpdateSpaceInput {
                space_id: created.space_id.clone(),
                name: row.patch_name.clone(),
                agent_core_evaluation_role_arn: row.patch_role.map(str::to_string),
                encryption_configuration: row.patch_encryption.map(encryption),
            })
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: UpdateSpace failed: {error:?}"))
            .space;

        assert_eq!(
            updated.name, row.expected_name,
            "case {case:?}: expected name {:?}, got {:?}",
            row.expected_name, updated.name
        );
        assert_eq!(
            updated.agent_core_evaluation_role_arn.as_deref(),
            row.expected_role,
            "case {case:?}: expected agentCoreEvaluationRoleArn {:?}, got {:?}",
            row.expected_role,
            updated.agent_core_evaluation_role_arn
        );
        let expected_encryption = (
            row.expected_encryption.0.to_string(),
            row.expected_encryption.1.map(str::to_string),
        );
        let actual_encryption = reported_encryption(case, &updated);
        assert_eq!(
            actual_encryption, expected_encryption,
            "case {case:?}: expected encryptionConfiguration {expected_encryption:?}, got \
             {actual_encryption:?}"
        );
        assert_eq!(
            untouched(&updated),
            untouched(&created),
            "case {case:?}: a member UpdateSpace's input cannot name was changed"
        );
        // `updatedAt` moves when a member of the space moves, so a request carrying no
        // updatable member leaves it exactly where the create left it.
        let requested_anything = row.patch_name.is_some()
            || row.patch_role.is_some()
            || row.patch_encryption.is_some();
        if requested_anything {
            assert!(
                updated.updated_at.secs() >= created.updated_at.secs(),
                "case {case:?}: updatedAt {:?} precedes the value the create left, {:?}",
                updated.updated_at,
                created.updated_at
            );
        } else {
            assert_eq!(
                (updated.updated_at.secs(), updated.updated_at.subsec_nanos()),
                (created.updated_at.secs(), created.updated_at.subsec_nanos()),
                "case {case:?}: a patch carrying no member moved updatedAt to {:?}",
                updated.updated_at
            );
        }

        // The patched record is the stored one: the readers answer from it rather than
        // from a second copy.
        let fetched = omni
            .get_space(&created.space_id)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: GetSpace failed: {error:?}"))
            .space;
        assert_eq!(
            format!("{fetched:?}"),
            format!("{updated:?}"),
            "case {case:?}: GetSpace did not return the record UpdateSpace persisted"
        );
        let listed = omni.listed_name(case, &created.space_id).await;
        assert_eq!(
            listed, row.expected_name,
            "case {case:?}: ListSpaces reports name {listed:?}, expected {:?}",
            row.expected_name
        );
    }
}

#[tokio::test]
async fn an_update_is_idempotent_and_further_patches_compose() {
    // `UpdateSpace` is modelled `@idempotent`: repeating one patch answers the same space,
    // and a later patch of another member keeps what the earlier one set.
    let omni = Omni::new();
    let parent = omni.domain("omni-update-space-repeat").await;
    let created = omni
        .create_space("compose", &parent, "space-compose", None, None)
        .await;

    let rename = UpdateSpaceInput {
        space_id: created.space_id.clone(),
        name: Some("space-composed".to_string()),
        ..Default::default()
    };
    let first = omni
        .update_space(&rename)
        .await
        .expect("the rename succeeds")
        .space;
    let repeated = omni
        .update_space(&rename)
        .await
        .expect("repeating the rename succeeds")
        .space;
    assert_eq!(
        format!("{repeated:?}"),
        format!("{first:?}"),
        "an @idempotent patch replayed must answer the same space, got {repeated:?}"
    );

    let then = omni
        .update_space(&UpdateSpaceInput {
            space_id: created.space_id.clone(),
            agent_core_evaluation_role_arn: Some(AGENT_ROLE_ARN.to_string()),
            ..Default::default()
        })
        .await
        .expect("adding the role succeeds")
        .space;
    assert_eq!(
        (then.name.as_str(), then.agent_core_evaluation_role_arn.as_deref()),
        ("space-composed", Some(AGENT_ROLE_ARN)),
        "a later patch dropped what the earlier one set: {then:?}"
    );
}

#[tokio::test]
async fn an_inconsistent_encryption_configuration_is_refused_and_changes_nothing() {
    let omni = Omni::new();
    let parent = omni.domain("omni-update-space-encryption").await;
    let created = omni
        .create_space(
            "encryption",
            &parent,
            "space-encryption",
            Some(AGENT_ROLE_ARN),
            Some((CUSTOMER_MANAGED, Some(CUSTOMER_KEY_ARN))),
        )
        .await;
    let before = format!("{created:?}");

    // `EncryptionConfiguration`'s model doc: `kmsKeyArn` is required for CUSTOMER_MANAGED
    // and must be absent for AWS_OWNED; "a mismatch is a ValidationException rather than a
    // silently ignored field".
    let cases: Vec<(&str, Encryption)> = vec![
        ("customer-managed-without-key", (CUSTOMER_MANAGED, None)),
        (
            "aws-owned-with-key",
            (AWS_OWNED, Some(CUSTOMER_KEY_ARN)),
        ),
    ];
    for (case, configuration) in cases {
        expect_modelled_error(
            case,
            omni.update_space(&UpdateSpaceInput {
                space_id: created.space_id.clone(),
                // The rename rides along, so the refusal is shown to roll back the whole
                // patch rather than only the encryption member.
                name: Some("space-encryption-renamed".to_string()),
                encryption_configuration: Some(encryption(configuration)),
                ..Default::default()
            })
            .await,
            400,
            "ValidationException",
        );
        let after = omni
            .get_space(&created.space_id)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: GetSpace failed: {error:?}"))
            .space;
        assert_eq!(
            format!("{after:?}"),
            before,
            "case {case:?}: a refused UpdateSpace changed the space"
        );
    }
}

#[tokio::test]
async fn updating_a_space_the_store_does_not_hold_is_not_found() {
    let omni = Omni::new();
    // Well-formed `SpaceId`s that name nothing: an id the store never held, and one whose
    // space was never created in this store at all.
    for space_id in [
        MISSING_SPACE_ID,
        "ffffffff-ffff-4fff-bfff-ffffffffffff",
    ] {
        expect_modelled_error(
            space_id,
            omni.update_space(&UpdateSpaceInput {
                space_id: space_id.to_string(),
                name: Some("space-missing".to_string()),
                ..Default::default()
            })
            .await,
            404,
            "ResourceNotFoundException",
        );
    }

    // A space created after the misses still updates: a miss is not a write.
    let parent = omni.domain("omni-update-space-missing").await;
    let created = omni
        .create_space("after misses", &parent, "space-after-misses", None, None)
        .await;
    let updated = omni
        .update_space(&UpdateSpaceInput {
            space_id: created.space_id.clone(),
            name: Some("space-after-misses-renamed".to_string()),
            ..Default::default()
        })
        .await
        .expect("the space created after the misses updates")
        .space;
    assert_eq!(
        updated.name, "space-after-misses-renamed",
        "the update after the misses did not persist, got {:?}",
        updated.name
    );
}

#[tokio::test]
async fn the_modelled_update_constraints_refuse_what_they_declare() {
    // `aws_smithy_schema_server::validate_to_message` is the exact check the framework
    // runs on a deserialized input before the handler sees it, so the handler never
    // re-checks a modelled constraint.
    let too_long = "c".repeat(65);
    let refused: Vec<(&str, String)> = vec![
        ("shorter than @length(min: 3)", "ab".to_string()),
        ("longer than @length(max: 64)", too_long),
        ("outside the name pattern (upper case)", "Upper".to_string()),
        (
            "outside the name pattern (consecutive hyphens)",
            "double--hyphen".to_string(),
        ),
        (
            "outside the name pattern (trailing hyphen)",
            "trailing-".to_string(),
        ),
    ];
    for (why, name) in refused {
        let input = UpdateSpaceInput {
            space_id: MISSING_SPACE_ID.to_string(),
            name: Some(name.clone()),
            ..Default::default()
        };
        assert!(
            aws_smithy_schema_server::validate_to_message(&input, UPDATE_SPACE_INPUT_SCHEMA)
                .is_some(),
            "case {why:?}: UpdateSpace name {name:?} was accepted"
        );
    }
    // Both published bounds of `name`'s @length(3..64) are accepted.
    for name in ["abc".to_string(), "b".repeat(64)] {
        let input = UpdateSpaceInput {
            space_id: MISSING_SPACE_ID.to_string(),
            name: Some(name.clone()),
            ..Default::default()
        };
        assert_eq!(
            aws_smithy_schema_server::validate_to_message(&input, UPDATE_SPACE_INPUT_SCHEMA),
            None,
            "case {name:?}: a name on the published bound was refused"
        );
    }
    // `spaceId` carries `SpaceId`'s UUID pattern.
    let input = UpdateSpaceInput {
        space_id: "not-a-uuid".to_string(),
        ..Default::default()
    };
    assert!(
        aws_smithy_schema_server::validate_to_message(&input, UPDATE_SPACE_INPUT_SCHEMA).is_some(),
        "a spaceId outside the modelled pattern was accepted"
    );
}

#[tokio::test]
async fn update_space_over_the_rpcv2cbor_wire() {
    let omni = Omni::new();
    let parent = omni.domain("omni-update-space-wire").await;
    let created = omni
        .create_space(
            "wire",
            &parent,
            "space-wire",
            Some(AGENT_ROLE_ARN),
            Some((CUSTOMER_MANAGED, Some(CUSTOMER_KEY_ARN))),
        )
        .await;

    let codec = CborCodec::default();
    let (status, body) = omni
        .over_the_wire(
            "UpdateSpace",
            UPDATE_SPACE_INPUT_SCHEMA,
            &UpdateSpaceInput {
                space_id: created.space_id.clone(),
                name: Some("space-wire-renamed".to_string()),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        status,
        200,
        "UpdateSpace over the wire answered {status}: {:?}",
        String::from_utf8_lossy(&body)
    );
    let mut deserializer = codec.create_deserializer(&body);
    let patched = UpdateSpaceOutput::deserialize(&mut deserializer)
        .expect("the output deserializes")
        .space;
    assert_eq!(
        (
            patched.name.as_str(),
            patched.agent_core_evaluation_role_arn.as_deref(),
            reported_encryption("wire", &patched),
        ),
        (
            "space-wire-renamed",
            Some(AGENT_ROLE_ARN),
            (
                CUSTOMER_MANAGED.to_string(),
                Some(CUSTOMER_KEY_ARN.to_string())
            ),
        ),
        "the wire response did not keep the members the patch omitted: {patched:?}"
    );

    // And a miss over the wire is the modelled 404 with its modelled error code.
    let (status, body) = omni
        .over_the_wire(
            "UpdateSpace",
            UPDATE_SPACE_INPUT_SCHEMA,
            &UpdateSpaceInput {
                space_id: MISSING_SPACE_ID.to_string(),
                name: Some("space-wire-missing".to_string()),
                ..Default::default()
            },
        )
        .await;
    let rendered = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        status, 404,
        "UpdateSpace over the wire answered {status} for a missing space: {rendered:?}"
    );
    assert!(
        rendered.contains("ResourceNotFoundException"),
        "the wire error body does not name ResourceNotFoundException: {rendered:?}"
    );
}
