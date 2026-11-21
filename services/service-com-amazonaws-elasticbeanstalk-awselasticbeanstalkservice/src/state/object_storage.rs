// SPDX-License-Identifier: CC0-1.0

//! The account's OBJECT STORAGE, as this service reaches it: the Elastic
//! Beanstalk storage bucket, the saved-configuration documents and the
//! environment-information files it holds, and the reads of a caller-named
//! archive a create builds its resource from.
//!
//! Split out of [`super`] so the one in-process Amazon S3 client and everything
//! written through or read from it live together.

use super::*;

/// The Amazon S3 bucket Elastic Beanstalk stores an account's application
/// versions, logs and other environment files in, in `region`:
/// `elasticbeanstalk-<region>-<account>`, the form `CreateStorageLocation`
/// reports. One bucket per account per region, so the name is DERIVED rather
/// than generated: "If the storage location already exists,
/// `CreateStorageLocation` still returns the bucket name but does not create a
/// new bucket."
pub fn storage_bucket_name(region: &str) -> String {
    format!(
        "{ARN_NAMESPACE}-{region}-{}",
        ::engine::emulator_account_id()
    )
}

/// The address of the stored configuration template document, as the awsQuery
/// `CreateConfigurationTemplate` and `UpdateConfigurationTemplate` results
/// report it:
/// `<this emulator's address>/<bucket>/resources/templates/<application>/<template>`.
///
/// RECORDED BEHAVIOUR, not modelled: `ConfigurationSettingsDescription` has no
/// `TemplateUrl` member on disk, and the responses the real service answers a
/// template write with carry one. It is DERIVED, not stored: the bucket is the
/// account's own storage location in this region ([`storage_bucket_name`]) and
/// the key is the templates prefix followed by the application and the template
/// the request named ([`configuration_template_key`]), so two reports of one
/// template agree and the value tracks the names a client supplied.
///
/// `endpoint` is the base URL this emulator is REACHED on
/// (`OperationContext::endpoint`), not Amazon S3's production hostname: the
/// member is the location a client FETCHES the template document from, and the
/// document this service stored lives in THIS emulator's object storage, so a
/// URL on any other authority leads a client away from it (to real AWS, an
/// egress refusal or a name that does not resolve) and never to the template
/// the emulator holds. The rest of the URL is object storage's path-style
/// bucket-and-key form, so it addresses the very object
/// [`store_configuration_template_document`] wrote; host and port are the
/// environment's, being reachable on this emulator is the requirement.
pub fn configuration_template_url(
    endpoint: &str,
    region: &str,
    application: &str,
    template: &str,
) -> String {
    format!(
        "{}/{}/{}",
        endpoint.trim_end_matches('/'),
        storage_bucket_name(region),
        configuration_template_key(application, template)
    )
}

/// The Amazon S3 key the account's storage bucket holds the saved configuration
/// `template` of `application` at — the path part of
/// [`configuration_template_url`], and the key the document is written to and
/// deleted from, so the reported location and the stored object cannot name two
/// different places.
pub fn configuration_template_key(application: &str, template: &str) -> String {
    format!("{CONFIGURATION_TEMPLATE_PREFIX}{application}/{template}")
}

/// Write `template`'s saved-configuration document into the account's Elastic
/// Beanstalk storage bucket, at the key [`configuration_template_url`] reports.
///
/// A configuration template IS a document in that bucket — "resources/templates"
/// under the storage location `CreateStorageLocation` creates — and the write
/// that creates or updates the template is what puts it there, so the URL the
/// write reports addresses an object the emulator's own object storage really
/// holds rather than a location nothing wrote. The write goes over the
/// in-process callback (see [`s3_client`]), exactly as the compiled environment
/// information's does.
///
/// Every failure is logged and the write is still reported: an emulator
/// assembled without object storage, or one that refused the write, answers
/// nothing about the template this service recorded — the same treatment
/// [`compile_environment_info`] gives that class of answers.
pub async fn store_configuration_template_document(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    template: &ConfigurationTemplate,
) {
    let bucket = storage_bucket_name(region);
    let key = configuration_template_key(&template.application_name, &template.template_name);
    // The document is stored where the account's Elastic Beanstalk files are
    // stored, so the bucket has to be there first; the create is idempotent.
    if let Err(refusal) = create_storage_bucket(ctx, region, &bucket).await {
        ::tracing::warn!(
            bucket,
            region,
            refusal = ?refusal,
            "object storage would not stand up the Elastic Beanstalk storage bucket, so the \
             saved configuration document could not be stored in it",
        );
    }
    if let Err(failure) = s3_client(ctx, region)
        .put_object()
        .bucket(&bucket)
        .key(&key)
        .body(::aws_sdk_s3::primitives::ByteStream::from(
            configuration_template_document(template).into_bytes(),
        ))
        .send()
        .await
    {
        ::tracing::warn!(
            bucket,
            key = %key,
            detail = %::aws_smithy_types::error::display::DisplayErrorContext(&failure),
            "object storage did not store the saved configuration document",
        );
    }
}

/// Delete the saved-configuration document of the template `template` of
/// `application` from the account's storage bucket — the object
/// [`store_configuration_template_document`] wrote.
///
/// A deleted saved configuration has no document to fetch, so the location the
/// writes reported stops resolving when the template it named is gone.
pub async fn discard_configuration_template_document(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    application: &str,
    template: &str,
) {
    let bucket = storage_bucket_name(region);
    let key = configuration_template_key(application, template);
    if let Err(failure) = s3_client(ctx, region)
        .delete_object()
        .bucket(&bucket)
        .key(&key)
        .send()
        .await
    {
        ::tracing::warn!(
            bucket,
            key = %key,
            detail = %::aws_smithy_types::error::display::DisplayErrorContext(&failure),
            "object storage did not delete the saved configuration document",
        );
    }
}

/// The saved-configuration document of `template`: the YAML document Elastic
/// Beanstalk keeps a saved configuration as.
///
/// Its shape is the published saved-configuration format — the
/// `EnvironmentConfigurationMetadata` block (the description and the two
/// instants of the configuration), the `Platform` the configuration is for, the
/// `OptionSettings` the configuration holds grouped by namespace, and the
/// `EnvironmentTier` it is for. Everything in it is read off the stored record,
/// so the document a client downloads is the configuration this service holds.
///
/// Emitted by the YAML writer of the same crate this service reads caller
/// YAML with, so nothing serialises a document by hand.
pub(super) fn configuration_template_document(template: &ConfigurationTemplate) -> String {
    use ::yaml_rust2::{yaml::Hash, Yaml, YamlEmitter};

    let text = |value: &str| Yaml::String(value.to_string());
    let mut metadata = Hash::new();
    if let Some(description) = &template.description {
        metadata.insert(text("Description"), text(description));
    }
    metadata.insert(
        text("DateCreated"),
        text(&epoch_millis(&template.date_created)),
    );
    metadata.insert(
        text("DateModified"),
        text(&epoch_millis(&template.date_updated)),
    );

    // The settings the configuration itself holds, grouped the way the document
    // carries them: one block per namespace, each option in it at its value.
    let mut namespaces: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for setting in &template.option_settings {
        let (namespace, option_name, _) = option_setting_key(setting);
        if namespace.is_empty() || option_name.is_empty() {
            continue;
        }
        namespaces
            .entry(namespace)
            .or_default()
            .insert(option_name, setting.value.clone().unwrap_or_default());
    }
    let mut options = Hash::new();
    for (namespace, settings) in &namespaces {
        let mut block = Hash::new();
        for (option_name, value) in settings {
            block.insert(text(option_name), text(value));
        }
        options.insert(text(namespace), Yaml::Hash(block));
    }

    let mut document = Hash::new();
    document.insert(
        text("EnvironmentConfigurationMetadata"),
        Yaml::Hash(metadata),
    );
    if let Some(platform_arn) = &template.platform_arn {
        let mut platform = Hash::new();
        platform.insert(text("PlatformArn"), text(platform_arn));
        document.insert(text("Platform"), Yaml::Hash(platform));
    } else if let Some(solution_stack_name) = &template.solution_stack_name {
        let mut platform = Hash::new();
        platform.insert(text("SolutionStackName"), text(solution_stack_name));
        document.insert(text("Platform"), Yaml::Hash(platform));
    }
    document.insert(text("OptionSettings"), Yaml::Hash(options));
    // "Templates aren't associated with any environment", so the document is
    // for the tier every configuration starts in.
    let tier = environment_tier_with_defaults(None);
    let mut tier_block = Hash::new();
    if let Some(name) = &tier.name {
        tier_block.insert(text("Name"), text(name));
    }
    if let Some(tier_type) = &tier.r#type {
        tier_block.insert(text("Type"), text(tier_type));
    }
    document.insert(text("EnvironmentTier"), Yaml::Hash(tier_block));

    let mut rendered = String::new();
    if let Err(failure) = YamlEmitter::new(&mut rendered).dump(&Yaml::Hash(document)) {
        ::tracing::warn!(
            detail = %failure,
            "the saved configuration document could not be written as YAML",
        );
    }
    rendered
}

/// `at` as the epoch milliseconds a saved-configuration document states an
/// instant in, through the timestamp type's OWN conversion, so nothing here
/// repeats the seconds-and-nanoseconds arithmetic. An instant outside the range
/// milliseconds can hold states the epoch instead of a wrapped number.
pub(super) fn epoch_millis(at: &::aws_smithy_types::DateTime) -> String {
    at.to_millis().unwrap_or_default().to_string()
}

/// The key prefix the account's storage bucket holds saved configurations under.
/// See [`configuration_template_url`].
pub(super) const CONFIGURATION_TEMPLATE_PREFIX: &str = "resources/templates/";

/// The Amazon S3 error codes that say the object a read named IS NOT THERE: the
/// bucket does not exist, or the key does not. `NoSuchBucket` and `NoSuchKey` are the
/// codes Amazon S3's own model declares for a read
/// (`smithy_query(query="operation", operation="GetObject")` on the S3 model), and
/// `NotFound` is what a HEAD — which carries no error document to name them in —
/// reports instead. All three are answered with HTTP 404.
pub(super) const ABSENT_ARCHIVE_CODES: &[&str] = &["NoSuchBucket", "NoSuchKey", "NoSuchVersion", "NotFound"];

/// The HTTP status Amazon S3 answers a read of an object that IS NOT THERE with —
/// an absent bucket and an absent key are both a 404 — and the status it answers a
/// read it does not PERMIT with. These two are what the service can conclude
/// something about the caller's archive from; every other answer says nothing about
/// it (see [`archive_download_refusal`]).
pub(super) const ARCHIVE_ABSENT_STATUS: u16 = 404;
/// See [`ARCHIVE_ABSENT_STATUS`].
pub(super) const ARCHIVE_DENIED_STATUS: u16 = 403;

/// The Amazon S3 error codes that say the read of the OBJECT was not PERMITTED, from
/// Amazon S3's published REST error-code list (both are answered with HTTP 403).
///
/// A 403 is only about the archive when it is one of these: a code about the
/// CREDENTIALS the read was signed with (`InvalidAccessKeyId`,
/// `SignatureDoesNotMatch`, and the emulator's own authentication refusal) says
/// nothing about the caller's object, so it falls to the logged branch in
/// [`archive_download_refusal`] rather than telling a client its archive is
/// unreadable.
pub(super) const DENIED_ARCHIVE_CODES: &[&str] = &["AccessDenied", "AllAccessDisabled"];

/// The reason a refusal carries when the failed read named no HTTP status to take a
/// reason phrase from: an archive the service could not read is, at minimum, not
/// there.
pub(super) const ARCHIVE_NOT_FOUND_REASON: &str = "Not Found";

/// The IAM action the archive read is authorized as: `HeadObject` "needs the
/// s3:GetObject permission for this operation" (Amazon S3 API reference), which is
/// also the permission the download of the archive itself needs.
pub(super) const ARCHIVE_READ_ACTION: &str = "s3:GetObject";

/// The ARN of the Amazon S3 object `key` in `bucket` — the resource the archive read
/// is authorized against. An Amazon S3 object ARN carries no region and no account
/// segment: `arn:aws:s3:::<bucket>/<key>`.
pub(super) fn archive_object_arn(bucket: &str, key: &str) -> String {
    format!("arn:aws:s3:::{bucket}/{key}")
}

/// The message Elastic Beanstalk refuses a create whose Amazon S3 archive it could
/// not download with, built from the request:
///
/// ```text
/// Unable to download from S3 location (Bucket: <bucket>  Key: <key>). Reason: <reason>
/// ```
///
/// Two spaces before `Key:`, as the recorded `CreateApplicationVersion` and
/// `CreatePlatformVersion` refusals carry it. The bucket and the key are the
/// request's own, and the reason is the HTTP reason phrase of the Amazon S3 read
/// that failed.
pub fn unable_to_download_message(bucket: &str, key: &str, reason: &str) -> String {
    format!("Unable to download from S3 location (Bucket: {bucket}  Key: {key}). Reason: {reason}")
}

/// The message Elastic Beanstalk refuses a create whose Amazon S3 archive the read
/// was NOT PERMITTED to fetch with, built from the request:
///
/// ```text
/// Unable to download from S3 location (Bucket: <bucket>  Key: <key>). Reason: Forbidden: S3Bucket=<bucket>, S3Key=<key>
/// ```
///
/// The same sentence [`unable_to_download_message`] builds, with the reason phrase
/// of the 403 such a read answers, and the Amazon S3 location the refused read
/// named stated after it — as the recorded refusal of a forbidden archive read
/// carries it, which is what tells a client WHICH object it may not read. Both
/// members are the request's own (`S3Location$S3Bucket`, `S3Location$S3Key`), so
/// nothing of any one run is written into it.
pub fn unable_to_read_archive_message(bucket: &str, key: &str) -> String {
    format!(
        "{}: S3Bucket={bucket}, S3Key={key}",
        unable_to_download_message(bucket, key, archive_reason(Some(ARCHIVE_DENIED_STATUS)))
    )
}

/// The Amazon S3 client an operation reads a caller-named archive's metadata through,
/// over the in-process callback into this same emulator.
///
/// The callback is the framework's documented seam for calling another emulated
/// service: it routes in process, so no network client is configured here and this
/// crate names no other service's crate. `region` is the request's own signing
/// region, so the read happens where the caller's own Amazon S3 requests go.
pub(super) fn s3_client(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_s3::Client {
    let config = ::aws_sdk_s3::Config::builder()
        .behavior_version(::aws_sdk_s3::config::BehaviorVersion::latest())
        .region(::aws_sdk_s3::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://s3.{region}.amazonaws.com"))
        // The in-process transport routes on the request's own path, and a
        // virtual-host bucket name is not a host this emulator resolves, so the
        // request addresses its bucket in the path.
        .force_path_style(true)
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .http_client(ctx.callback().http_client())
        // The transport is in-process: there is nothing to retry, time out or watch
        // for a stalled stream, and each of those would need a sleep implementation
        // this client is not given. The same goes for caching the identity: the
        // credentials are handed over directly.
        .retry_config(::aws_sdk_s3::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_s3::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_s3::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_s3::config::StalledStreamProtectionConfig::disabled())
        .build();
    ::aws_sdk_s3::Client::from_conf(config)
}

/// Whether an Amazon S3 answer to a read of a caller-named object says anything
/// about that OBJECT: a 404 (the bucket or the key is not there) and a 403
/// carrying one of [`DENIED_ARCHIVE_CODES`] do; anything else says nothing about
/// it — an emulator assembled without object storage answers the router's own
/// `UnknownService`, an authentication refusal is about the signer, and a
/// transport failure answers nothing at all.
///
/// The one statement of that classification, so the create's existence check
/// ([`archive_download_refusal`]) and the pre-processing that reads the same
/// archive ([`version_processing::process_source_bundle`]) cannot disagree about
/// which answers are about the caller's archive.
pub(crate) fn answer_is_about_the_object(status: Option<u16>, code: &str) -> bool {
    match status {
        Some(ARCHIVE_ABSENT_STATUS) => ABSENT_ARCHIVE_CODES.contains(&code),
        Some(ARCHIVE_DENIED_STATUS) => DENIED_ARCHIVE_CODES.contains(&code),
        _ => false,
    }
}

/// The refusal message for the Amazon S3 archive at `location`, or `None` when object
/// storage says the archive is there and readable.
///
/// Elastic Beanstalk builds a resource FROM the archive a create names
/// (`CreateApplicationVersion$SourceBundle`, `CreatePlatformVersion$PlatformDefinitionBundle`),
/// so it reads that archive while serving the create and refuses the request when it
/// cannot — which is why a typo'd bucket or key is a 400 at the create rather than a
/// resource a client believes is deployable. The read goes to object storage, in
/// process: nothing here decides from the SHAPE of the bucket or key whether the
/// archive exists.
///
/// It is a `HeadObject`, not a `GetObject`: "a HEAD request has the same options as a
/// GET operation on an object" and answers the same existence and authorization
/// verdict, with NO body — so whether the archive can be read is settled without ever
/// materialising a caller-sized object in this process. Nothing here interprets the
/// archive's contents, so its bytes are never needed. Exactly one such read per
/// request, no loop, nothing retained.
///
/// The read is a read: it changes nothing a later caller sees, in this service's
/// store or in object storage.
///
/// A refusal is only ever grounded in an answer about the OBJECT, which is what the
/// two statuses below are: 404 (the bucket or the key is not there) and 403 carrying
/// one of [`DENIED_ARCHIVE_CODES`]. Anything else says nothing about the caller's
/// archive — an emulator assembled without object storage answers the router's own
/// `UnknownService`, an authentication refusal is about the signer, and a transport
/// failure answers nothing at all — so it is logged and the create proceeds.
///
/// The read is made for the CALLER, so it is the caller's own permissions that decide
/// whether it may be made at all: a caller whose policies do not allow reading that
/// object is answered the refusal object storage's own 403 produces, and the read is
/// not made. The decision is the one shared gate's
/// ([`crate::service::authorization::permits`]) — this emulator's in-process callback
/// signs with the emulator's own credential rather than the caller's, so asking the
/// caller's policies here is what keeps the read attributed to the caller.
pub async fn archive_download_refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    location: &S3Location,
) -> Option<String> {
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

    let bucket = location.s3_bucket.as_deref().unwrap_or_default();
    let key = location.s3_key.as_deref().unwrap_or_default();
    // A location that names no bucket, or no key, names no Amazon S3 object at all:
    // there is nothing to read and nothing to ask object storage about.
    if bucket.is_empty() || key.is_empty() {
        return Some(unable_to_download_message(
            bucket,
            key,
            ARCHIVE_NOT_FOUND_REASON,
        ));
    }
    if !crate::service::authorization::permits(
        engine,
        ctx,
        ARCHIVE_READ_ACTION,
        &archive_object_arn(bucket, key),
    ) {
        return Some(unable_to_read_archive_message(bucket, key));
    }
    let failure = match s3_client(ctx, region)
        .head_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
    {
        // The archive is there and readable, so the create goes ahead.
        Ok(_) => return None,
        Err(failure) => failure,
    };
    let status = failure
        .raw_response()
        .map(|response| response.status().as_u16());
    let code = failure.code().unwrap_or_default().to_string();
    if !answer_is_about_the_object(status, &code) {
        ::tracing::warn!(
            bucket,
            key,
            code = %code,
            status = ?status,
            detail = %::aws_smithy_types::error::display::DisplayErrorContext(&failure),
            "the Amazon S3 read of a caller-named archive answered nothing about the object, \
             so the create was not refused for it",
        );
        return None;
    }
    // ONE sentence per condition: a read object storage would not permit is the
    // same refusal whether the caller's own policies settled it above or object
    // storage answered the 403 itself, and every other answer about the object
    // carries the reason phrase of the status it answered (404 -> `Not Found`),
    // read off that status rather than written out per error code.
    if status == Some(ARCHIVE_DENIED_STATUS) {
        return Some(unable_to_read_archive_message(bucket, key));
    }
    Some(unable_to_download_message(
        bucket,
        key,
        archive_reason(status),
    ))
}

/// The reason phrase a failed archive read's refusal carries: the HTTP reason phrase
/// of the status the read answered (404 -> `Not Found`, 403 -> `Forbidden`), so it is
/// read off the status rather than written out per error code.
pub(super) fn archive_reason(status: Option<u16>) -> &'static str {
    status
        .and_then(|status| ::http::StatusCode::from_u16(status).ok())
        .and_then(|status| status.canonical_reason())
        .unwrap_or(ARCHIVE_NOT_FOUND_REASON)
}

/// The HTTP status Amazon S3 answers a `CreateBucket` whose name the one bucket
/// namespace already holds with: `BucketAlreadyOwnedByYou` and `BucketAlreadyExists`
/// — the two errors `CreateBucket` models — are both 409.
pub(super) const BUCKET_EXISTS_STATUS: u16 = 409;

/// The Region an Amazon S3 `CreateBucket` that carries NO location constraint places
/// its bucket in, from `CreateBucketConfiguration$LocationConstraint`'s own
/// documentation: "If you don't specify a Region, the bucket is created in the US East
/// (N. Virginia) Region (us-east-1) by default." It is also why
/// `BucketLocationConstraint` enumerates every other Region code but not this one, so
/// a bucket in this Region is created with no configuration at all.
pub(super) const UNCONSTRAINED_BUCKET_REGION: &str = "us-east-1";

/// Amazon S3's published REST error code for an account that has reached its bucket
/// limit — the refusal `CreateStorageLocation`'s modelled `TooManyBucketsException`
/// ("The specified account has reached its limit of Amazon S3 buckets") is about.
pub(super) const TOO_MANY_BUCKETS_CODE: &str = "TooManyBuckets";

/// Why object storage would not stand the account's Elastic Beanstalk storage bucket
/// up, when its answer said something about the bucket at all. Each variant is the
/// modelled `CreateStorageLocation` error the caller is owed for it; see
/// [`create_storage_bucket`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBucketRefusal {
    /// The create was not PERMITTED (HTTP 403 carrying one of
    /// [`DENIED_ARCHIVE_CODES`]) — `InsufficientPrivilegesException`.
    NotPermitted,
    /// The account is at its Amazon S3 bucket limit — `TooManyBucketsException`.
    TooManyBuckets,
}

/// Create the account's Elastic Beanstalk storage bucket named `bucket` in `region`
/// in object storage, unless it is already there.
///
/// `CreateStorageLocation` "creates a bucket in Amazon S3 to store application
/// versions, logs, and other files used by Elastic Beanstalk environments", and the
/// bucket it reports is the one a client then uploads its source bundle into — so the
/// bucket has to EXIST in the emulator's object storage, which is Amazon S3's own
/// service, not merely be named in this service's store. The create goes over the
/// in-process callback (see [`s3_client`]), so the bucket a client addresses is the
/// bucket object storage really holds.
///
/// IDEMPOTENT, which is the operation's own contract ("if the storage location already
/// exists, `CreateStorageLocation` still returns the bucket name but does not create a
/// new bucket"): a name the one bucket namespace already holds answers 409, and this
/// emulator serves one account, so that bucket is the account's own storage location
/// and the call reports it.
///
/// A refusal is only ever grounded in an answer about the BUCKET: a 403 carrying one of
/// [`DENIED_ARCHIVE_CODES`] (the permission-refusal codes Amazon S3 answers any request
/// with, read off the same published list the archive read uses) and the bucket-limit
/// code. Anything else says nothing about whether object storage holds the bucket — an
/// emulator assembled without object storage answers the router's own `UnknownService`,
/// an authentication refusal is about the signer, a transport failure answers nothing at
/// all — so it is logged and the storage location is still reported, the way
/// [`archive_download_refusal`] treats the same class of answers.
pub async fn create_storage_bucket(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    bucket: &str,
) -> Result<(), StorageBucketRefusal> {
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

    // One storage location per account per REGION, so the bucket is placed in the
    // request's own region — which on `CreateBucket` is said with the location
    // constraint, not with the signing region. See [`UNCONSTRAINED_BUCKET_REGION`] for
    // the one region that is said by saying nothing.
    let mut create = s3_client(ctx, region).create_bucket().bucket(bucket);
    if region != UNCONSTRAINED_BUCKET_REGION {
        create = create.create_bucket_configuration(
            ::aws_sdk_s3::types::CreateBucketConfiguration::builder()
                .location_constraint(::aws_sdk_s3::types::BucketLocationConstraint::from(region))
                .build(),
        );
    }
    let failure = match create.send().await {
        // The bucket is there now.
        Ok(_) => return Ok(()),
        Err(failure) => failure,
    };
    let status = failure
        .raw_response()
        .map(|response| response.status().as_u16());
    let code = failure.code().unwrap_or_default().to_string();
    match (status, code.as_str()) {
        // The bucket was already there, so there was nothing to create and the
        // storage location is reported as it stands.
        (Some(BUCKET_EXISTS_STATUS), _) => Ok(()),
        (Some(ARCHIVE_DENIED_STATUS), code) if DENIED_ARCHIVE_CODES.contains(&code) => {
            Err(StorageBucketRefusal::NotPermitted)
        }
        // Amazon S3's bucket quota is far above this service's own
        // [`MAX_STORAGE_LOCATIONS`], so this is the account having spent its buckets
        // elsewhere rather than on storage locations.
        (_, TOO_MANY_BUCKETS_CODE) => Err(StorageBucketRefusal::TooManyBuckets),
        _ => {
            ::tracing::warn!(
                bucket,
                region,
                code = %code,
                status = ?status,
                detail = %::aws_smithy_types::error::display::DisplayErrorContext(&failure),
                "the Amazon S3 create of the account's Elastic Beanstalk storage bucket \
                 answered nothing about the bucket, so the storage location was reported \
                 as it stands",
            );
            Ok(())
        }
    }
}

/// The Amazon S3 key the information compiled from one of an environment's
/// instances is stored at in the account's Elastic Beanstalk storage bucket.
///
/// `CreateStorageLocation` creates the bucket Elastic Beanstalk "store[s]
/// application versions, LOGS, and other files used by Elastic Beanstalk
/// environments" in, so that bucket is where a compiled file goes, under a key
/// naming what was compiled, for which environment and from which instance. The
/// layout below is this emulator's own: the model documents only that the file
/// is an Amazon S3 object the `Message` URL names, never where it sits — and the
/// file name is `eb-activity.log`, the environment activity log the compiled
/// content IS (see [`ComAmazonawsElasticbeanstalkAwselasticbeanstalkserviceState::environment_activity_log`]).
pub fn environment_info_key(
    info_type: &str,
    environment_id: &str,
    ec2_instance_id: &str,
) -> String {
    format!(
        "resources/environments/logs/{info_type}/{environment_id}/{ec2_instance_id}/eb-activity.log"
    )
}

/// One compiled file of an environment's information: the instance it was
/// compiled from, where object storage holds it, and the URL a caller downloads
/// it at.
#[derive(Debug, Clone)]
pub struct CompiledEnvironmentInfoFile {
    pub ec2_instance_id: String,
    pub s3_key: String,
    /// The presigned Amazon S3 URL of that object — `EnvironmentInfoDescription`'s
    /// `Message`, which "currently contains a presigned Amazon S3 URL". `None`
    /// when object storage did not store the file or the URL could not be
    /// signed: an entry then carries no `Message` rather than a URL to
    /// something a caller cannot download.
    pub message: Option<String>,
}

/// What compiling one environment's information produced: the bucket it was
/// stored in, and one file per instance.
#[derive(Debug, Clone, Default)]
pub struct CompiledEnvironmentInfo {
    pub bucket: String,
    pub files: Vec<CompiledEnvironmentInfoFile>,
}

/// A compile whose files the clock has deleted: the objects object storage must
/// forget.
#[derive(Debug, Clone)]
pub struct DiscardedEnvironmentInfo {
    pub bucket: String,
    pub s3_keys: Vec<String>,
}

/// The signing name of the object storage a compiled file's download URL
/// addresses: the URL names an object in a bucket, which is a request to Amazon
/// S3, so that is the service the presigned URL is signed for and the service
/// the emulator routes a caller that follows it to.
pub(super) const OBJECT_STORAGE_SIGNING_NAME: &str = "s3";

/// Compile `log` into the account's Elastic Beanstalk storage bucket, one object
/// per instance in `ec2_instance_ids`, and sign a download URL for each.
///
/// This is the work `RequestEnvironmentInfo` "initiates": the compiled file is a
/// REAL object in the emulator's own object storage, written over the in-process
/// callback (see [`s3_client`]), and the `Message` a later
/// `RetrieveEnvironmentInfo` reports is a presigned `GetObject` URL of it — so a
/// client that follows the URL downloads the bytes this service stored, and no
/// URL names an object nothing wrote.
///
/// The URL is signed against the base URL this emulator is REACHED on, through
/// the framework's own URL signer: a `Message` is an address the CALLER follows,
/// so it has to name a host this emulator answers on — no production Amazon S3
/// hostname resolves to it. It is signed for [`ENVIRONMENT_INFO_RETENTION`],
/// which is how long the files live ("the files are deleted after 15 minutes"),
/// so the URL stops working at the instant the file it names is deleted.
///
/// Every failure is answered by compiling fewer files rather than by failing the
/// request: an emulator assembled without object storage, or one that refused
/// the write, leaves the entry with no `Message`, which is what
/// [`CompiledEnvironmentInfoFile::message`] records.
pub async fn compile_environment_info(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    environment_id: &str,
    info_type: &str,
    ec2_instance_ids: &[String],
    log: &str,
) -> CompiledEnvironmentInfo {
    let bucket = storage_bucket_name(region);
    if ec2_instance_ids.is_empty() {
        return CompiledEnvironmentInfo {
            bucket,
            ..Default::default()
        };
    }
    // The compiled files are stored where the account's Elastic Beanstalk files
    // are stored, so the bucket has to be there; a refusal is logged and the
    // writes below then say, per file, whether anything was stored.
    if let Err(refusal) = create_storage_bucket(ctx, region, &bucket).await {
        ::tracing::warn!(
            bucket,
            region,
            refusal = ?refusal,
            "object storage would not stand up the Elastic Beanstalk storage bucket, so the \
             compiled environment information could not be stored in it",
        );
    }

    let mut files = Vec::with_capacity(ec2_instance_ids.len());
    for ec2_instance_id in ec2_instance_ids {
        let s3_key = environment_info_key(info_type, environment_id, ec2_instance_id);
        let stored = s3_client(ctx, region)
            .put_object()
            .bucket(&bucket)
            .key(&s3_key)
            .body(::aws_sdk_s3::primitives::ByteStream::from(
                log.as_bytes().to_vec(),
            ))
            .send()
            .await;
        let message = match stored {
            Ok(_) => presigned_download(ctx, region, &bucket, &s3_key),
            Err(failure) => {
                ::tracing::warn!(
                    bucket,
                    key = %s3_key,
                    detail = %::aws_smithy_types::error::display::DisplayErrorContext(&failure),
                    "object storage did not store the compiled environment information, so the \
                     entry reports no download URL",
                );
                None
            }
        };
        files.push(CompiledEnvironmentInfoFile {
            ec2_instance_id: ec2_instance_id.clone(),
            s3_key,
            message,
        });
    }

    CompiledEnvironmentInfo { bucket, files }
}

/// Delete the objects of every compile the clock has deleted — the housekeeping
/// behind "the files are deleted after 15 minutes".
///
/// Called by the two operations that have a request context to reach object
/// storage through, with the records
/// [`ComAmazonawsElasticbeanstalkAwselasticbeanstalkserviceState::take_deleted_environment_info`]
/// handed back. Nothing a caller can observe depends on it: a deleted compile is
/// never reported, and its presigned URL expired at the same instant.
pub async fn discard_compiled_environment_info(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    discarded: &[DiscardedEnvironmentInfo],
) {
    for compile in discarded {
        for key in &compile.s3_keys {
            if let Err(failure) = s3_client(ctx, region)
                .delete_object()
                .bucket(&compile.bucket)
                .key(key)
                .send()
                .await
            {
                ::tracing::warn!(
                    bucket = %compile.bucket,
                    key = %key,
                    detail = %::aws_smithy_types::error::display::DisplayErrorContext(&failure),
                    "object storage did not delete a compiled environment information file",
                );
            }
        }
    }
}

/// The presigned `GetObject` URL of `bucket`/`key`, valid as long as the file it
/// names lives, or `None` when the URL could not be signed.
///
/// Path-style, the way object storage addresses a bucket and a key in a URL, and
/// signed through the framework's URL signer — which signs for the base URL this
/// emulator is reached on, so the address leads back to the object this service
/// really stored rather than to a host only the real Amazon S3 answers.
pub(super) fn presigned_download(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    bucket: &str,
    key: &str,
) -> Option<String> {
    let path = format!("/{bucket}/{key}");
    let request = ::smithy_server_runtime::SignedUrlRequest::new(
        ::smithy_server_runtime::http::Method::GET,
        OBJECT_STORAGE_SIGNING_NAME,
        &path,
    )
    .region(region);
    match ctx.url_signer().sign(request, ENVIRONMENT_INFO_RETENTION) {
        Ok(signed) => Some(signed.as_str().to_string()),
        Err(failure) => {
            ::tracing::warn!(
                bucket,
                key,
                detail = %failure,
                "the compiled environment information file's download URL could not be signed",
            );
            None
        }
    }
}
