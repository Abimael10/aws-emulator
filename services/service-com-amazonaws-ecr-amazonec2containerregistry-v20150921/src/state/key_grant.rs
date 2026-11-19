// SPDX-License-Identifier: CC0-1.0

//! The KMS grant a repository encrypted with a CUSTOMER MANAGED key holds on that key.
//!
//! # The rule this module implements
//!
//! A repository whose `encryptionConfiguration` names a KMS key has its contents encrypted
//! under a key the ACCOUNT owns, and this service cannot use another principal's key on the
//! strength of its own say-so: the authorization has to be one KMS itself holds. So creating
//! such a repository takes GRANTS on the named key — for the service principal this service
//! calls KMS as ([`service_principal`]), limited to the data-key operations encrypting and
//! decrypting that repository's contents needs ([`GRANT_OPERATIONS`]), and constrained to the
//! one repository they were taken for (`GrantConstraints$SourceArn`, "the ARN of an Amazon Web
//! Services resource on behalf of which the request is made", whose constraint "ensures that
//! the principal can use the KMS key only when the request is made on behalf of the specified
//! resource", together with the encryption context that use carries,
//! [`REPOSITORY_ENCRYPTION_CONTEXT_KEY`]) — and deleting that repository RETIRES them, because
//! an authorization outlives nothing it was taken for. `kms:ListGrants` on the key is then the
//! record of which service holds what authorization on it, which is what a key owner audits
//! and revokes against.
//!
//! The create issues that request [`GRANTS_PER_REPOSITORY`] times, and the grants carry no
//! `Name`, so the key ends up carrying that many records for the repository — alike in
//! everything the request states and told apart by their own `GrantId`: "when this value is
//! absent, all CreateGrant requests result in a new grant with a unique GrantId even if all
//! the supplied parameters are identical"
//! (`smithy_query(query="doc",shape="CreateGrantRequest",member="Name",model_path="services/service-com-amazonaws-kms-trentservice/smithy-model/com-amazonaws-kms-trentservice.json")`).
//!
//! Only a key the request NAMES is granted on: `AES256` "encrypts the images in the
//! repository using an AES256 encryption algorithm" with Amazon S3-managed keys, and a `KMS`
//! or `KMS_DSSE` configuration naming no key uses "the default Amazon Web Services managed KMS
//! key for Amazon ECR" (`smithy_query(query="shape",shape="EncryptionConfiguration")`), which
//! is not the account's key to authorize on.
//!
//! # How KMS is reached
//!
//! The way every emulated service reaches another: the AWS SDK driven over the framework's
//! in-process callback ([`smithy_server_runtime::OperationContext::callback`]), through the
//! tree's one construction of that client (`service_common::kms_key::kms_client`). No socket
//! is opened, no name is resolved, nothing outside this process is reachable, and the
//! credentials are the emulator's own — the caller's `Authorization` header is never
//! forwarded. Every call is made with NO registry lock held.
//!
//! # What an answer from KMS establishes
//!
//! A grant this service could not take is not a repository that is encrypted anyway, so a
//! DEFINITE negative — KMS answering, with a client status, about the key itself — is the
//! modelled `KmsException` ("the operation failed due to a KMS exception", with `kmsError`
//! "the error code returned by KMS"), and nothing is created. What the caller is told is the
//! answer KMS gave — its own sentence and the clause naming the answer it came from
//! ([`relayed_message`]) — because the reason the create failed is one only KMS holds.
//! Anything else establishes nothing about the key: an emulator assembled with no KMS beside
//! this service (the front door's own refusal, `service_common::in_process_refusal`), a
//! fault, a refusal of the call itself. Such an answer refuses nothing and takes no grant,
//! and it is logged, because a deployment with no KMS to ask is one where this half of the
//! contract does not run.

use ::aws_sdk_kms::error::ProvideErrorMetadata;
use ::aws_smithy_runtime_api::client::orchestrator::HttpResponse;

/// How many grants one KMS-encrypted repository takes on the key it is encrypted with.
///
/// TWO, as AWS's own wire carries them: in the recorded units of this check
/// (`ecr__kms.kms-encrypted-repository-creates-exactly-one-ecr-grant-and-retires-it-on-delete`)
/// a key encrypting one repository is reported with two grants, alike in everything the
/// creating request states and told apart only by the `GrantId` KMS minted for each (the
/// grants carry no `Name`; see the module docs) and the instant each was created. So the create
/// issues its request that many times. Why the service takes two rather than one is its own
/// business and nothing a client can read; what a key owner auditing the key sees is the count,
/// which is what this reproduces.
pub const GRANTS_PER_REPOSITORY: usize = 2;

/// The KMS operations one of these grants permits, as the key's own grant list reports them.
///
/// Exactly the two the recorded units of this contract carry — `Operations
/// ["Decrypt","GenerateDataKey"]`, as the report of this issue's sibling records them
/// (`issues/.../20261024T182018Z-ecr-kms-ecr-createrepository-with-encryptiontype-kms-creates-no-kms.md`,
/// the same trace from an earlier corpus) — which is what encrypting a repository's contents
/// under the key needs and no more: ECR asks KMS for a data key under the CMK for the contents
/// it stores, and decrypts that data key to read them back. A grant is "a policy instrument
/// that allows Amazon Web Services principals to use KMS keys in cryptographic operations", so
/// nothing that is not such a use is in it — in particular no management operation, which a
/// grant taken to encrypt one repository has no reason to carry.
pub const GRANT_OPERATIONS: [&str; 2] = ["Decrypt", "GenerateDataKey"];

/// The encryption-context key the repository's own ARN is passed to KMS under, and therefore
/// the one the grant's `EncryptionContextSubset` constraint is written against.
///
/// Amazon ECR's encryption at rest uses the encrypted repository's ARN as its encryption
/// context, under the `aws:ecr:arn` key, which is what confines a grant — and a key policy
/// written against it — to ONE repository: "allow the permissions in the grant only when the
/// encryption context in the request … includes (EncryptionContextSubset) the encryption
/// context specified in the constraint"
/// (`smithy_query(query="doc",shape="CreateGrantRequest",member="Constraints",model_path="services/service-com-amazonaws-kms-trentservice/smithy-model/com-amazonaws-kms-trentservice.json")`).
const REPOSITORY_ENCRYPTION_CONTEXT_KEY: &str = "aws:ecr:arn";

/// The service principal this service calls KMS as, for `region`.
///
/// The regional form, because the grant is taken at one Region's endpoint on a key that lives
/// in that Region: an AWS service principal is `<service>.<region>.amazonaws.com` where the
/// service's use of the key is regional, and this service's namespace is the one its ARNs
/// carry (`ecr`).
#[must_use]
pub fn service_principal(region: &str) -> String {
    format!("ecr.{region}.amazonaws.com")
}

/// One grant a repository holds on the customer managed key its contents are encrypted under.
///
/// Both members are what KMS answered: the key's own ARN as `kms:DescribeKey` reports it
/// (`KeyMetadata$Arn`) and the `GrantId` `kms:CreateGrant` minted. They are kept together
/// because retiring the grant names both — "to identify the grant to retire, use a grant
/// token, or both the grant ID and a key identifier of the KMS key" — and a grant token is a
/// credential, so none is stored.
#[derive(Debug, Clone)]
pub struct KeyGrant {
    /// The ARN of the key the grant is on, as KMS reports the key.
    pub key_arn: String,
    /// The `GrantId` KMS minted for the grant.
    pub grant_id: String,
}

/// What came of taking the grants a KMS-encrypted repository needs.
#[derive(Debug, Clone)]
pub enum Taken {
    /// KMS holds these grants for the repository.
    Grants(Vec<KeyGrant>),
    /// KMS answered about the key itself, with a client status, and no grant was taken:
    /// the code KMS returned and the sentence it came with.
    Refused {
        /// The error code KMS returned, which is what `KmsException$kmsError` carries.
        kms_error: String,
        /// The sentence KMS answered with, which states what about the key was wrong.
        message: String,
    },
    /// KMS established nothing about the key (see the module docs), so nothing is refused and
    /// no grant is held.
    Unknown,
}

/// The key a repository encrypted as `encryption` needs a grant on, or `None` when it needs
/// none: the S3-managed encryption type takes no key of the account's, and a KMS type naming
/// no key uses the Amazon Web Services managed key for Amazon ECR.
#[must_use]
pub fn granted_key(encryption: &crate::ops::types::EncryptionConfiguration) -> Option<&str> {
    if encryption.encryption_type == super::S3_MANAGED_ENCRYPTION_TYPE {
        return None;
    }
    encryption
        .kms_key
        .as_deref()
        .filter(|key| !key.trim().is_empty())
}

/// Take the grants `repository_arn`'s contents need on the key `kms_key` names, at `region`'s
/// KMS endpoint.
///
/// The key is RESOLVED first (`kms:DescribeKey`, the read that "provides detailed information
/// about a KMS key"): a grant names the key it was taken on by the key's own ARN, which is
/// also the identifier the grant is retired by, and the request may name the key by an alias
/// or a key id instead — "the alias, key ID, or full ARN of the KMS key can be specified"
/// (`smithy_query(query="shape",shape="EncryptionConfiguration")`).
///
/// [`GRANTS_PER_REPOSITORY`] grants are then taken, one request each. An answer KMS gives
/// about the key refuses the whole create, so any grant already taken is retired again rather
/// than left on the key for a repository that is not created.
pub async fn take(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kms_key: &str,
    repository_arn: &str,
) -> Taken {
    let Some(client) = ::service_common::kms_key::kms_client(ctx, region) else {
        return Taken::Unknown;
    };
    let described = match client.describe_key().key_id(kms_key).send().await {
        Ok(described) => described,
        Err(error) => return answered_about_the_key(&error, "resolve", kms_key),
    };
    let Some(key_arn) = described
        .key_metadata
        .and_then(|metadata| metadata.arn)
        .filter(|arn| !arn.is_empty())
    else {
        // KMS answered about the key but did not say WHICH key it is, so there is nothing
        // to take a grant on by the key's own identifier.
        ::tracing::warn!(
            kms_key = ?kms_key,
            "KMS reported no ARN for the key this repository names, so no grant was taken on it"
        );
        return Taken::Unknown;
    };

    let mut taken: Vec<KeyGrant> = Vec::with_capacity(GRANTS_PER_REPOSITORY);
    while taken.len() < GRANTS_PER_REPOSITORY {
        match one_grant(&client, region, &key_arn, repository_arn).await {
            Ok(grant) => taken.push(grant),
            Err(unserved) => {
                // Nothing is left holding permission on the key for a create that is not
                // going through.
                for grant in &taken {
                    retire_one(&client, grant).await;
                }
                return unserved;
            }
        }
    }
    Taken::Grants(taken)
}

/// Take ONE of the grants, on the key already resolved to `key_arn`: the grant KMS minted, or
/// the reason there is none.
async fn one_grant(
    client: &::aws_sdk_kms::Client,
    region: &str,
    key_arn: &str,
    repository_arn: &str,
) -> Result<KeyGrant, Taken> {
    let mut request = client
        .create_grant()
        .key_id(key_arn)
        .grantee_service_principal(service_principal(region))
        // "When you specify a GranteeServicePrincipal, you must also specify a SourceArn
        // grant constraint. In addition, you must specify either a RetiringPrincipal or a
        // RetiringServicePrincipal" — and the principal that retires this grant is the one
        // that took it, since deleting the repository is what retires it.
        .retiring_service_principal(service_principal(region))
        .constraints(
            ::aws_sdk_kms::types::GrantConstraints::builder()
                // The repository the grant was taken for, in both terms KMS confines a
                // grant by: the resource the key may be used on behalf of, and the
                // encryption context that use carries
                // ([`REPOSITORY_ENCRYPTION_CONTEXT_KEY`]). Neither is the caller's to
                // choose — both are this repository's own ARN — so the grant permits the
                // key's use for this repository and for nothing else.
                .source_arn(repository_arn)
                .encryption_context_subset(
                    REPOSITORY_ENCRYPTION_CONTEXT_KEY,
                    repository_arn,
                )
                .build(),
        );
    for operation in GRANT_OPERATIONS {
        request = request.operations(::aws_sdk_kms::types::GrantOperation::from(operation));
    }
    let created = match request.send().await {
        Ok(created) => created,
        Err(error) => {
            return Err(answered_about_the_key(&error, "take a grant on", key_arn));
        }
    };
    match created.grant_id {
        Some(grant_id) if !grant_id.is_empty() => Ok(KeyGrant {
            key_arn: key_arn.to_string(),
            grant_id,
        }),
        // A grant KMS reported without an id is one nothing can retire, so it is not
        // recorded as one this repository holds.
        _ => {
            ::tracing::warn!(
                key_arn = ?key_arn,
                "KMS reported no GrantId for the grant this repository needs on its key"
            );
            Err(Taken::Unknown)
        }
    }
}

/// Retire every grant in `grants`, at `region`'s KMS endpoint.
///
/// Best effort and reported nowhere: the repository they were taken for is gone either way,
/// and a grant KMS no longer holds — one a key owner revoked, one on a key whose deletion is
/// scheduled — is already retired as far as this service is concerned.
pub async fn retire(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    grants: &[KeyGrant],
) {
    if grants.is_empty() {
        return;
    }
    let Some(client) = ::service_common::kms_key::kms_client(ctx, region) else {
        return;
    };
    for grant in grants {
        retire_one(&client, grant).await;
    }
}

/// Retire ONE grant: the single `kms:RetireGrant` call of this module, so its two callers —
/// the delete, and the create backing out a refused request — make the same one.
async fn retire_one(client: &::aws_sdk_kms::Client, grant: &KeyGrant) {
    if let Err(error) = client
        .retire_grant()
        .key_id(&grant.key_arn)
        .grant_id(&grant.grant_id)
        .send()
        .await
    {
        ::tracing::warn!(
            key_arn = ?grant.key_arn,
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "the KMS grant this repository held on its key could not be retired"
        );
    }
}

/// How one failed KMS call is read: an answer KMS gave with a CLIENT status is its judgement
/// of a request that is entirely about the key — resolving it, or taking a grant on it — and
/// is relayed as the modelled `KmsException`; anything else establishes nothing (see the
/// module docs). A 5xx is about KMS rather than about the key, whatever code travels with it.
///
/// `attempted` names what was being done to the key, for the operator-visible line a
/// non-answer leaves behind.
fn answered_about_the_key<E>(
    error: &::aws_sdk_kms::error::SdkError<E, HttpResponse>,
    attempted: &str,
    kms_key: &str,
) -> Taken
where
    E: ProvideErrorMetadata + ::std::error::Error + 'static,
{
    let answered = error
        .raw_response()
        .filter(|response| response.status().is_client_error());
    // The front door's OWN refusal of a call that reached no service — an emulator with no KMS
    // mounted beside this one — is not KMS answering about the key, whatever status it carries
    // (`service_common::in_process_refusal`).
    let reached_nothing = error
        .raw_response()
        .and_then(|response| response.body().bytes())
        .is_some_and(::service_common::in_process_refusal::is_transport_refusal);
    let code = ProvideErrorMetadata::code(error).unwrap_or_default();
    if let (Some(answered), false, false) = (answered, code.is_empty(), reached_nothing) {
        return Taken::Refused {
            kms_error: code.to_string(),
            message: relayed_message(
                ProvideErrorMetadata::message(error).unwrap_or_default(),
                code,
                answered.status().as_u16(),
                &::service_common::relayed_refusal::answered_request_id(answered.headers()),
            ),
        };
    }
    // Visible to an operator: an answer nothing can be read from is not a judgement about
    // the key, so without this line it would be indistinguishable from a key that was
    // granted on. The caller's own value is rendered in its escaped `Debug` form, so a key
    // identifier carrying a newline cannot forge a log line.
    ::tracing::warn!(
        kms_key = ?kms_key,
        attempted = attempted,
        error = %::aws_smithy_types::error::display::DisplayErrorContext(error),
        "KMS gave no answer about this key, so no grant was taken on it"
    );
    Taken::Unknown
}

/// The service name a relayed KMS answer names itself by, which is KMS's own.
const KMS_SERVICE_NAME: &str = "AWSKMS";

/// The package the caught KMS exception's class is named under, which is KMS's own: an answer
/// KMS refused a call with is reported as `com.amazonaws.services.kms.model.<ErrorCode>`, the
/// way the SDK layer that made the call renders the exception it caught.
const KMS_EXCEPTION_PACKAGE: &str = "com.amazonaws.services.kms.model";

/// What this service answers as the `KmsException`'s `message` for an answer KMS gave about
/// the key: the caught KMS exception's fully qualified class, KMS's OWN sentence, and the
/// clause naming the answer it came from — which service answered, the status it answered
/// with, its error code, the identifier of that answer and the proxy it came through,
/// rendered by the workspace's one rendering of that envelope
/// (`service_common::relayed_refusal::caught_exception_message`).
///
/// EVERY part of it comes from KMS's answer: the class from the code it answered under, the
/// sentence from the answer itself, the status from the answer's own status line and the
/// identifier from its headers. This service states no opening of its own in front of them — a
/// relaying service that has one states fixed text, and nothing available here (the model,
/// which says only that `KmsException` is "the operation failed due to a KMS exception" with
/// `kmsError` carrying "the error code returned by KMS", nor any recorded answer of this
/// service) states what that text is for Amazon ECR, so a token worded here would be this
/// service inventing the dependency's wording rather than relaying it.
fn relayed_message(message: &str, code: &str, status: u16, request_id: &str) -> String {
    ::service_common::relayed_refusal::caught_exception_message(
        "",
        KMS_EXCEPTION_PACKAGE,
        message,
        ::service_common::relayed_refusal::CaughtCall {
            service: KMS_SERVICE_NAME,
            status,
            code,
            request_id,
        },
    )
}
