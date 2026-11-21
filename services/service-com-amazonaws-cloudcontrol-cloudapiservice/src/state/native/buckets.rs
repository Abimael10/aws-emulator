// SPDX-License-Identifier: CC0-1.0

//! `AWS::S3::Bucket`, as it lives in the Amazon S3 service's own store.
//!
//! A bucket is a resource of the Amazon S3 service: `s3:CreateBucket` creates it,
//! `s3:HeadBucket` and `s3:ListBuckets` report it, `s3:PutBucketTagging` and
//! `s3:PutBucketVersioning` change it and `s3:DeleteBucket` removes it, and the record of
//! it lives in that service's state in the same shared engine. So this provider holds a
//! handle to THAT store and nothing of its own: a bucket this emulator holds is one Cloud
//! Control API resolves whichever API created it, a bucket Amazon S3 no longer holds is
//! one Cloud Control API reports as not found, and a bucket a `CreateResource` provisioned
//! is one the S3 API itself serves objects in — which is the whole point of provisioning
//! it through a uniform control plane.
//!
//! A bucket's name is "unique across all AWS accounts in all the AWS Regions within a
//! partition" (Amazon S3 User Guide, *General purpose bucket naming rules*), so the owning
//! service holds its buckets in ONE namespace and a reference resolves to the same bucket
//! whichever Region the call naming it was signed for; the Region a create was signed for
//! is the Region the bucket is then placed in.
//!
//! Only the buckets of the caller's own S3 account are resources of this type: a bucket
//! another emulated service provisioned as the object store of one of ITS resources, and a
//! bucket AWS publishes, are not the account's — which is exactly the distinction that
//! service's own `ListBuckets` reader draws
//! (`s3::ComAmazonawsS3Amazons3State::general_purpose_account_buckets`), so this provider
//! resolves and enumerates through it.
//!
//! The resource model is the type's registry schema. Catalog source: the `AWS::S3::Bucket`
//! resource provider schema's `properties`, whose `primaryIdentifier` is
//! `/properties/BucketName`. The properties this path APPLIES are the identity, the
//! bucket's `Tags`, its `VersioningConfiguration`, its `PublicAccessBlockConfiguration` and
//! its `BucketEncryption`, each carried by the owning record and each read and written
//! through that service's own rules; the read-only `Arn` is reported beside them.
//!
//! Every other property the schema declares ([`UNAPPLIED_PROPERTIES`]) is one THIS path
//! cannot apply, so a model that asks for one is refused rather than accepted and dropped —
//! a property a caller sets has to be one a later read reports. Each of them is a
//! subresource the owning service installs with a call of its own (`PutBucketEncryption`,
//! `PutBucketLifecycleConfiguration`, `PutBucketNotificationConfiguration`, …) rather than a
//! field of the bucket's record, or a value that service derives rather than stores.

use ::std::time::SystemTime;

use ::serde_json::{Map, Value};
use ::service_com_amazonaws_s3_amazons3::ops::types::BUCKET_VERSIONING_STATUS_VALUES;
use ::service_com_amazonaws_s3_amazons3::ops::types::SERVER_SIDE_ENCRYPTION_VALUES;
use ::service_com_amazonaws_s3_amazons3::state as s3;
use ::service_com_amazonaws_s3_amazons3::state::{BucketEncryptionRule, BucketPublicAccessBlock};
use ::service_com_amazonaws_s3tables_s3tablebuckets::state as s3tables;

use super::{require_value, text, NativeRefusal, NativeResourceProvider, S3State};

/// The `TypeName` this provider stands behind.
pub const TYPE_NAME: &str = "AWS::S3::Bucket";

/// The property that carries the type's primary identifier, which the schema's
/// `primaryIdentifier` points at (`/properties/BucketName`): the bucket's name, which the
/// caller chooses.
const IDENTIFIER_PROPERTY: &str = "BucketName";

/// The read-only property of the type a read reports beside the identity: the bucket's ARN
/// (one of the schema's `readOnlyProperties`), built by the owning service's own ARN
/// builder.
const ARN: &str = "Arn";

/// The bucket's tag set (`Tags`), a list of `{ Key, Value }` pairs.
const TAGS: &str = "Tags";

/// The key member of one entry of [`TAGS`].
const TAG_KEY: &str = "Key";

/// The value member of one entry of [`TAGS`].
const TAG_VALUE: &str = "Value";

/// The bucket's versioning state (`VersioningConfiguration`), the container
/// `s3:PutBucketVersioning` carries.
const VERSIONING_CONFIGURATION: &str = "VersioningConfiguration";

/// The bucket's Block Public Access settings (`PublicAccessBlockConfiguration`), the
/// container `s3:PutPublicAccessBlock` carries.
///
/// The owning service applies them WITHOUT client input: "Amazon S3 automatically enables S3
/// Block Public Access … for new buckets", so a bucket carries a configuration whether or
/// not any desired state named one, and every read of the model reports it.
const PUBLIC_ACCESS_BLOCK_CONFIGURATION: &str = "PublicAccessBlockConfiguration";

/// The four members of [`PUBLIC_ACCESS_BLOCK_CONFIGURATION`], each a `Setting` (a boolean) —
/// so a member the configuration does not turn on is off, which is how the owning service's
/// own record of them reads.
const PUBLIC_ACCESS_BLOCK_SETTINGS: [&str; 4] = [
    "BlockPublicAcls",
    "BlockPublicPolicy",
    "IgnorePublicAcls",
    "RestrictPublicBuckets",
];

/// The bucket's default encryption (`BucketEncryption`), the container
/// `s3:PutBucketEncryption` carries.
///
/// The owning service applies it WITHOUT client input too: "by default, all buckets have a
/// default encryption configuration that uses server-side encryption with Amazon S3 managed
/// keys", so a bucket has one whether or not any desired state named it.
const BUCKET_ENCRYPTION: &str = "BucketEncryption";

/// The one member of [`BUCKET_ENCRYPTION`]: the rules that configuration is made of.
const ENCRYPTION_RULES: &str = "ServerSideEncryptionConfiguration";

/// The member of one rule that carries the encryption applied by default.
const ENCRYPTION_BY_DEFAULT: &str = "ServerSideEncryptionByDefault";

/// The algorithm member of [`ENCRYPTION_BY_DEFAULT`], which that container requires.
const SSE_ALGORITHM: &str = "SSEAlgorithm";

/// The key member of [`ENCRYPTION_BY_DEFAULT`] — allowed "if and only if SSEAlgorithm is set
/// to aws:kms or aws:kms:dsse".
const KMS_MASTER_KEY_ID: &str = "KMSMasterKeyID";

/// The S3 Bucket Key member of one rule.
const BUCKET_KEY_ENABLED: &str = "BucketKeyEnabled";

/// The one member of [`VERSIONING_CONFIGURATION`] this path applies: the versioning
/// `Status`.
const VERSIONING_STATUS: &str = "Status";

/// The properties of the type's schema THIS path does not apply — see this module's header.
///
/// Named here so a model asking for one is told it is a property of the type this handler
/// does not apply, rather than one the type does not declare at all: the two are different
/// faults and a caller fixes them differently.
const UNAPPLIED_PROPERTIES: &[&str] = &[
    "AccelerateConfiguration",
    "AccessControl",
    "AnalyticsConfigurations",
    "CorsConfiguration",
    "DomainName",
    "DualStackDomainName",
    "IntelligentTieringConfigurations",
    "InventoryConfigurations",
    "LifecycleConfiguration",
    "LoggingConfiguration",
    "MetadataTableConfiguration",
    "MetricsConfigurations",
    "NotificationConfiguration",
    "ObjectLockConfiguration",
    "ObjectLockEnabled",
    "OwnershipControls",
    "RegionalDomainName",
    "ReplicationConfiguration",
    "WebsiteConfiguration",
    "WebsiteURL",
];

/// The Amazon S3 buckets of one engine, as Cloud Control API resources.
pub struct BucketProvider {
    /// The owning service's store. A handle, so every answer is read from the live store.
    state: ::engine::ServiceState<S3State>,
    /// The Amazon S3 Tables store beside it, for the one thing a bucket's deletion reaches
    /// outside its own service: the metadata tables of an S3 Metadata configuration whose
    /// creation had not yet fallen due go back to the service that holds them
    /// ([`s3::ComAmazonawsS3Amazons3State::release_removed_metadata_tables`]).
    tables: ::engine::ServiceState<s3tables::ComAmazonawsS3tablesS3tablebucketsState>,
}

impl BucketProvider {
    /// The provider for the Amazon S3 store THIS engine carries, or `None` when it carries
    /// none.
    pub fn for_engine<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<Self> {
        Some(Self {
            state: s3::service_state(engine)?,
            tables: s3tables::service_state(engine)?,
        })
    }
}

impl NativeResourceProvider for BucketProvider {
    fn type_name(&self) -> &'static str {
        TYPE_NAME
    }

    fn identifier_property(&self) -> &'static str {
        IDENTIFIER_PROPERTY
    }

    fn read(&self, region: &str, identifier: &str) -> Option<Value> {
        // One namespace, whichever Region the call was signed for: see this module's
        // header.
        let _ = region;
        let store = self.state.lock();
        account_bucket(&store, identifier).map(|bucket| model(identifier, bucket))
    }

    fn list(&self, region: &str, after: Option<&str>, limit: usize) -> Vec<(String, Value)> {
        let _ = region;
        let store = self.state.lock();
        // The owning service's own reader of "the buckets in your account" answers in
        // bucket-name order, so the window a listing pages over is that order resumed
        // after `after` — nothing is sorted here.
        store
            .general_purpose_account_buckets()
            .filter(|(name, _)| after.is_none_or(|after| *name > after))
            .take(limit)
            .map(|(name, bucket)| (name.to_string(), model(name, bucket)))
            .collect()
    }

    fn create(
        &self,
        region: &str,
        identifier: &str,
        properties: &Value,
        at: SystemTime,
    ) -> Result<Value, NativeRefusal> {
        // A bucket's record carries no instant a resource model moves.
        let _ = at;
        // The name a bucket is created under is the OWNING service's to accept: the
        // published general purpose bucket naming rules, read through the one shared
        // reading of them this workspace keeps, so nothing is created here under a name
        // that service's own API would refuse.
        if !::service_common::s3_bucket_name::is_well_formed(identifier) {
            return Err(NativeRefusal::Rejected(
                s3::INVALID_BUCKET_NAME_MESSAGE.to_string(),
            ));
        }
        // Read against what a bucket this create is about to make HOLDS: its own ARN, no
        // tag set yet, and the default encryption configuration the owning service applies
        // to every bucket. The ARN comes from the owning store's own builder for a general
        // purpose bucket — the one the record's `arn()` answers with once the bucket is
        // there ([`model`]) — because the model has to be judged before anything is
        // written, so there is no record to read it off yet.
        let desired = DesiredModel::read(
            identifier,
            properties,
            &s3::bucket_arn(identifier),
            &[],
            &[s3::default_encryption_rule()],
        )?;
        let mut store = self.state.lock();
        match store.create_bucket(identifier, region) {
            // A name the owning store already holds is that service's own taken-name
            // verdict, whoever holds the bucket under it — one bucket namespace.
            s3::BucketCreation::NameTaken => return Err(NativeRefusal::AlreadyExists),
            // Its account quota bounds what this create may add.
            s3::BucketCreation::QuotaReached => {
                return Err(NativeRefusal::AtCapacity(format!(
                    "The account already holds the maximum of {} buckets",
                    s3::MAX_BUCKETS_PER_ACCOUNT
                )));
            }
            s3::BucketCreation::Created => {}
        }
        let Some(bucket) = store.bucket_mut(identifier) else {
            // Unreachable: the create above recorded it, under this same guard.
            return Err(NativeRefusal::NotFound);
        };
        desired.apply(bucket);
        // The subresources a create installs come into being WITH the bucket rather than
        // being written onto it afterwards, which is what the owning record's own creation
        // bookkeeping says.
        bucket.forget_configuration_writes();
        Ok(model(identifier, bucket))
    }

    fn update(
        &self,
        region: &str,
        identifier: &str,
        properties: &Value,
        at: SystemTime,
    ) -> Result<Value, NativeRefusal> {
        // One namespace, and a bucket's record carries no instant a resource model moves.
        let _ = (region, at);
        let mut store = self.state.lock();
        // A bucket that is not the account's own is not a resource of this type, so there
        // is none of this name to patch.
        let held = account_bucket(&store, identifier).ok_or(NativeRefusal::NotFound)?;
        let (arn, in_force, encryption) = (
            held.arn(identifier),
            held.tags().to_vec(),
            held.effective_encryption_rules(),
        );
        let desired = DesiredModel::read(identifier, properties, &arn, &in_force, &encryption)?;
        let bucket = store
            .bucket_mut(identifier)
            .ok_or(NativeRefusal::NotFound)?;
        desired.apply(bucket);
        Ok(model(identifier, bucket))
    }

    fn delete(&self, region: &str, identifier: &str, at: SystemTime) -> Result<(), NativeRefusal> {
        let _ = region;
        let mut store = self.state.lock();
        let held = account_bucket(&store, identifier).ok_or(NativeRefusal::NotFound)?;
        // "All objects (including all object versions and delete markers) in the bucket
        // must be deleted before the bucket itself can be deleted" — the owning service
        // raises its own error about the bucket as it is, which the handler maps to no
        // verdict of its own, and the bucket stays.
        if !held.holds_no_versions() {
            let message = if held.versioning.is_some() {
                s3::BUCKET_NOT_EMPTY_VERSIONED_MESSAGE
            } else {
                s3::BUCKET_NOT_EMPTY_MESSAGE
            };
            return Err(NativeRefusal::Rejected(message.to_string()));
        }
        let region = held.region.clone();
        // The bucket goes, and the metadata tables of its configuration that no caller could
        // have seen in service go with it — to the service that holds them, which is the only
        // place they are. That store is taken UNDER this one's guard, the lock order the
        // owning service states.
        let removal = store.remove_bucket(identifier, at);
        let mut tables = self.tables.lock();
        let mut managed = s3::ManagedTableBucket::new(&mut tables, &region);
        store.release_removed_metadata_tables(&removal, &mut managed);
        Ok(())
    }
}

/// The bucket of the CALLER'S OWN S3 account named `identifier`, or `None` when that
/// service holds no such bucket of the account.
///
/// The one resolution this provider reads a bucket through, so a read, a patch and a delete
/// cannot disagree about which buckets are resources of this type.
fn account_bucket<'a>(store: &'a S3State, identifier: &str) -> Option<&'a s3::Bucket> {
    store
        .general_purpose_account_buckets()
        .find(|(name, _)| *name == identifier)
        .map(|(_, bucket)| bucket)
}

/// The resource model one bucket renders as — the schema's properties the owning record
/// carries, and only those it HAS: a bucket with no tag set and no versioning state carries
/// neither property, the same way the owning service's own readers report neither.
fn model(name: &str, bucket: &s3::Bucket) -> Value {
    let mut model = Map::new();
    model.insert(IDENTIFIER_PROPERTY.to_string(), Value::from(name));
    model.insert(ARN.to_string(), Value::from(bucket.arn(name)));
    if !bucket.tags().is_empty() {
        let tags: Vec<Value> = bucket
            .tags()
            .iter()
            .map(|tag| {
                Value::Object(Map::from_iter([
                    (TAG_KEY.to_string(), Value::from(tag.key.clone())),
                    (TAG_VALUE.to_string(), Value::from(tag.value.clone())),
                ]))
            })
            .collect();
        model.insert(TAGS.to_string(), Value::Array(tags));
    }
    if let Some(versioning) = &bucket.versioning {
        model.insert(
            VERSIONING_CONFIGURATION.to_string(),
            Value::Object(Map::from_iter([(
                VERSIONING_STATUS.to_string(),
                Value::from(versioning.status.clone()),
            )])),
        );
    }
    // The Block Public Access settings the OWNING service holds for the bucket, which it
    // applies to a new bucket without any client input: a read of the model reports what is
    // in force rather than only what a desired state set. A bucket whose configuration
    // `DeletePublicAccessBlock` has withdrawn carries none, which is the one state in which
    // the model reports none either.
    if let Some(settings) = bucket.public_access_block() {
        model.insert(
            PUBLIC_ACCESS_BLOCK_CONFIGURATION.to_string(),
            public_access_block_model(settings),
        );
    }
    // And the default encryption in force on it, which that service applies to every new
    // bucket as well: what its own `GetBucketEncryption` reports.
    model.insert(
        BUCKET_ENCRYPTION.to_string(),
        encryption_model(&bucket.effective_encryption_rules()),
    );
    Value::Object(model)
}

/// The `BucketEncryption` one bucket's default encryption rules render as: the rules of the
/// configuration, each with the encryption it applies by default and its Bucket Key setting.
fn encryption_model(rules: &[BucketEncryptionRule]) -> Value {
    let rendered: Vec<Value> = rules
        .iter()
        .map(|rule| {
            let mut entry = Map::new();
            if let Some(algorithm) = &rule.sse_algorithm {
                let mut by_default = Map::new();
                by_default.insert(SSE_ALGORITHM.to_string(), Value::from(algorithm.clone()));
                if let Some(key) = &rule.kms_master_key_id {
                    by_default.insert(KMS_MASTER_KEY_ID.to_string(), Value::from(key.clone()));
                }
                entry.insert(ENCRYPTION_BY_DEFAULT.to_string(), Value::Object(by_default));
            }
            if let Some(enabled) = rule.bucket_key_enabled {
                entry.insert(BUCKET_KEY_ENABLED.to_string(), Value::from(enabled));
            }
            Value::Object(entry)
        })
        .collect();
    Value::Object(Map::from_iter([(
        ENCRYPTION_RULES.to_string(),
        Value::Array(rendered),
    )]))
}

/// The `PublicAccessBlockConfiguration` one bucket's settings render as: each of the four
/// settings and whether it is on.
fn public_access_block_model(settings: BucketPublicAccessBlock) -> Value {
    Value::Object(Map::from_iter(
        PUBLIC_ACCESS_BLOCK_SETTINGS
            .into_iter()
            .zip(public_access_block_flags(settings))
            .map(|(member, on)| (member.to_string(), Value::from(on))),
    ))
}

/// The four settings of `configuration`, in the order [`PUBLIC_ACCESS_BLOCK_SETTINGS`] names
/// them — the one place the member order and the record's fields are lined up, so the reader
/// and the writer of the property cannot disagree about which member is which setting.
fn public_access_block_flags(configuration: BucketPublicAccessBlock) -> [bool; 4] {
    [
        configuration.block_public_acls,
        configuration.block_public_policy,
        configuration.ignore_public_acls,
        configuration.restrict_public_buckets,
    ]
}

/// What a resource model asks a bucket to be: the properties this path APPLIES, having been
/// checked against the owning service's own rules.
struct DesiredModel {
    /// The tag set the bucket is to carry.
    tags: Vec<s3::ResourceTag>,
    /// The versioning status the bucket is to be in, or `None` to leave it in the one it is
    /// in — which, for a bucket no configuration has been put on, is no versioning state at
    /// all.
    versioning_status: Option<String>,
    /// The Block Public Access settings the bucket is to carry, or `None` to leave it with
    /// the ones in force — which, for a bucket nothing has configured, are the ones the
    /// owning service applied when it created the bucket.
    public_access_block: Option<BucketPublicAccessBlock>,
    /// The default encryption rules the bucket is to carry, or `None` to leave it with the
    /// configuration in force — which, for a bucket nothing has configured, is the SSE-S3
    /// one the owning service applies to every bucket.
    encryption: Option<Vec<BucketEncryptionRule>>,
}

impl DesiredModel {
    /// Read `properties` as the model of the bucket `identifier`, whose own ARN is `arn` and
    /// whose tag set is `in_force` — or state why the owning service would not take it.
    ///
    /// The identity and the read-only ARN may be CARRIED (a read reports them, so a model a
    /// client round-trips carries them) but only with the value the bucket already answers
    /// to: a value accepted here and never applied would be one no later read reports.
    fn read(
        identifier: &str,
        properties: &Value,
        arn: &str,
        in_force: &[s3::ResourceTag],
        held_encryption: &[BucketEncryptionRule],
    ) -> Result<Self, NativeRefusal> {
        let Some(model) = properties.as_object() else {
            return Err(NativeRefusal::Invalid(format!(
                "{TYPE_NAME} requires a resource model of its properties"
            )));
        };
        let mut desired = Self {
            tags: Vec::new(),
            versioning_status: None,
            public_access_block: None,
            encryption: None,
        };
        for (property, value) in model {
            match property.as_str() {
                IDENTIFIER_PROPERTY => require_value(property, value, identifier)?,
                ARN => require_value(property, value, arn)?,
                TAGS => desired.tags = tag_set(value)?,
                VERSIONING_CONFIGURATION => {
                    desired.versioning_status = Some(versioning_status(value)?);
                }
                PUBLIC_ACCESS_BLOCK_CONFIGURATION => {
                    desired.public_access_block = Some(public_access_block(value)?);
                }
                BUCKET_ENCRYPTION => {
                    desired.encryption = Some(encryption_rules(value, held_encryption)?);
                }
                other if UNAPPLIED_PROPERTIES.contains(&other) => {
                    return Err(NativeRefusal::Invalid(format!(
                        "{other} is a property of {TYPE_NAME} this emulator's handler does \
                         not apply"
                    )));
                }
                other => {
                    return Err(NativeRefusal::Invalid(format!(
                        "{other} is not a property of {TYPE_NAME}"
                    )));
                }
            }
        }
        // The tag set the model leaves the bucket with is judged by the owning service's OWN
        // rule for a bucket's tag set, HERE rather than where the property is read, so the
        // set that rule sees is the one about to be stored whether the model carried the
        // property or left it out: a model naming no `Tags` asks for the empty set, which is
        // a tag-set write like any other. `in_force` is the set it replaces, which that rule
        // reads — a key in the namespace AWS keeps for its own tags may be stated back
        // unchanged but neither added nor updated by a requester, and a Cloud Control API
        // call is a requester's.
        s3::validate_tag_set(
            &desired.tags,
            s3::BUCKET_TAG_SET_LIMITS,
            s3::TagKeyOrigin::Caller,
            in_force,
        )
        .map_err(|rejection| NativeRefusal::Rejected(rejection.to_string()))?;
        Ok(desired)
    }

    /// Write this model onto `bucket`. A property the model does not carry is CLEARED where
    /// the owning service publishes a way to clear it — a tag set is replaced, so a model
    /// naming none leaves the bucket untagged — and left alone where what the bucket carries
    /// is that service's own rather than something a model set: the API has no call that
    /// takes a bucket back to having no versioning state at all, and the Block Public Access
    /// settings of a bucket are ones the service applies to every new bucket, so a model
    /// naming neither asks for nothing.
    fn apply(self, bucket: &mut s3::Bucket) {
        bucket.replace_tags(self.tags);
        if let Some(status) = self.versioning_status {
            bucket.set_versioning(&status, None);
        }
        if let Some(settings) = self.public_access_block {
            bucket.set_public_access_block(settings);
        }
        if let Some(rules) = self.encryption {
            bucket.set_encryption(rules);
        }
    }
}

/// The default encryption rules the `BucketEncryption` `value` names, or why the owning
/// service would not apply them.
///
/// `held` is the configuration in force on the bucket, which a rule's blocked encryption
/// types are carried over from: blocking one is a bucket-level setting the type's schema
/// declares no member for, so a model this path applies can neither state nor withdraw it.
fn encryption_rules(
    value: &Value,
    held: &[BucketEncryptionRule],
) -> Result<Vec<BucketEncryptionRule>, NativeRefusal> {
    let Some(entries) = value.get(ENCRYPTION_RULES).and_then(Value::as_array) else {
        return Err(NativeRefusal::Invalid(format!(
            "{BUCKET_ENCRYPTION} must carry {ENCRYPTION_RULES} as a list of rules"
        )));
    };
    // "A configuration associated with a bucket names at least one rule", which is what the
    // owning service's own writer refuses an empty list for.
    if entries.is_empty() {
        return Err(NativeRefusal::Rejected(format!(
            "{ENCRYPTION_RULES} must carry at least one rule"
        )));
    }
    let mut rules = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let Some(rule) = entry.as_object() else {
            return Err(NativeRefusal::Invalid(format!(
                "every {ENCRYPTION_RULES} entry must be a rule object"
            )));
        };
        let mut read = BucketEncryptionRule {
            sse_algorithm: None,
            kms_master_key_id: None,
            bucket_key_enabled: None,
            blocked_encryption_types: held
                .get(index)
                .and_then(|held| held.blocked_encryption_types.clone()),
        };
        for (member, carried) in rule {
            match member.as_str() {
                ENCRYPTION_BY_DEFAULT => {
                    let (algorithm, key) = encryption_by_default(carried)?;
                    read.sse_algorithm = Some(algorithm);
                    read.kms_master_key_id = key;
                }
                BUCKET_KEY_ENABLED => {
                    read.bucket_key_enabled = Some(carried.as_bool().ok_or_else(|| {
                        NativeRefusal::Invalid(format!("{BUCKET_KEY_ENABLED} must be a boolean"))
                    })?);
                }
                other => {
                    return Err(NativeRefusal::Invalid(format!(
                        "{other} is not a member of a {ENCRYPTION_RULES} rule"
                    )));
                }
            }
        }
        // "This parameter is allowed if and only if SSEAlgorithm is set to aws:kms or
        // aws:kms:dsse" — the owning service's own rule for a key named beside another
        // algorithm.
        if read.kms_master_key_id.is_some()
            && !read
                .sse_algorithm
                .as_deref()
                .is_some_and(|algorithm| s3::KMS_SSE_ALGORITHMS.contains(&algorithm))
        {
            return Err(NativeRefusal::Rejected(format!(
                "{KMS_MASTER_KEY_ID} is not applicable if {SSE_ALGORITHM} is not one of \
                 {:?}",
                s3::KMS_SSE_ALGORITHMS
            )));
        }
        rules.push(read);
    }
    if rules.len() > s3::MAX_SERVER_SIDE_ENCRYPTION_RULES {
        return Err(NativeRefusal::AtCapacity(format!(
            "{ENCRYPTION_RULES} carries {} rules, more than the {} this emulator records \
             for one bucket",
            rules.len(),
            s3::MAX_SERVER_SIDE_ENCRYPTION_RULES
        )));
    }
    Ok(rules)
}

/// The algorithm and the key the `ServerSideEncryptionByDefault` `value` names: the
/// algorithm is required of that container, and has to be one of the values the owning
/// service's model publishes.
fn encryption_by_default(value: &Value) -> Result<(String, Option<String>), NativeRefusal> {
    let Some(container) = value.as_object() else {
        return Err(NativeRefusal::Invalid(format!(
            "{ENCRYPTION_BY_DEFAULT} must be an object of its members"
        )));
    };
    let mut algorithm = None;
    let mut key = None;
    for (member, carried) in container {
        match member.as_str() {
            SSE_ALGORITHM => {
                let named = text(SSE_ALGORITHM, carried)?;
                if !SERVER_SIDE_ENCRYPTION_VALUES.contains(&named.as_str()) {
                    return Err(NativeRefusal::Invalid(format!(
                        "{SSE_ALGORITHM} must be one of {SERVER_SIDE_ENCRYPTION_VALUES:?}"
                    )));
                }
                algorithm = Some(named);
            }
            KMS_MASTER_KEY_ID => key = Some(text(KMS_MASTER_KEY_ID, carried)?),
            other => {
                return Err(NativeRefusal::Invalid(format!(
                    "{other} is not a member of {ENCRYPTION_BY_DEFAULT}"
                )));
            }
        }
    }
    let algorithm = algorithm.ok_or_else(|| {
        NativeRefusal::Invalid(format!(
            "{ENCRYPTION_BY_DEFAULT} requires {SSE_ALGORITHM}"
        ))
    })?;
    Ok((algorithm, key))
}

/// The Block Public Access settings the `PublicAccessBlockConfiguration` `value` names, or
/// why the owning service would not apply them.
///
/// Every member is a `Setting` — "Specifies whether Amazon S3 should block …" — so a member
/// the container leaves out is off, and a member carrying anything but a boolean is a
/// configuration that service cannot apply.
fn public_access_block(value: &Value) -> Result<BucketPublicAccessBlock, NativeRefusal> {
    let Some(container) = value.as_object() else {
        return Err(NativeRefusal::Invalid(format!(
            "{PUBLIC_ACCESS_BLOCK_CONFIGURATION} must be an object of its settings"
        )));
    };
    let mut flags = [false; 4];
    for (member, flag) in PUBLIC_ACCESS_BLOCK_SETTINGS.into_iter().zip(flags.iter_mut()) {
        match container.get(member) {
            Some(setting) => {
                *flag = setting.as_bool().ok_or_else(|| {
                    NativeRefusal::Invalid(format!(
                        "{PUBLIC_ACCESS_BLOCK_CONFIGURATION}.{member} must be a boolean"
                    ))
                })?;
            }
            None => *flag = false,
        }
    }
    for member in container.keys() {
        if !PUBLIC_ACCESS_BLOCK_SETTINGS.contains(&member.as_str()) {
            return Err(NativeRefusal::Invalid(format!(
                "{member} is not a setting of {PUBLIC_ACCESS_BLOCK_CONFIGURATION}"
            )));
        }
    }
    let [block_public_acls, block_public_policy, ignore_public_acls, restrict_public_buckets] =
        flags;
    Ok(BucketPublicAccessBlock {
        block_public_acls,
        ignore_public_acls,
        block_public_policy,
        restrict_public_buckets,
    })
}

/// The versioning status the `VersioningConfiguration` `value` names, or why the owning
/// service would not apply it.
///
/// "You can set the versioning state with one of the following values: Enabled … Suspended"
/// (`PutBucketVersioning`), which is the pair that service's own model declares
/// ([`BUCKET_VERSIONING_STATUS_VALUES`]); a container naming no status, or one outside that
/// pair, is a configuration it cannot apply.
fn versioning_status(value: &Value) -> Result<String, NativeRefusal> {
    let status = value
        .get(VERSIONING_STATUS)
        .map(|status| text(VERSIONING_STATUS, status))
        .transpose()?
        .filter(|status| BUCKET_VERSIONING_STATUS_VALUES.contains(&status.as_str()))
        .ok_or_else(|| {
            NativeRefusal::Invalid(format!(
                "{VERSIONING_CONFIGURATION}.{VERSIONING_STATUS} must be one of \
                 {BUCKET_VERSIONING_STATUS_VALUES:?}"
            ))
        })?;
    Ok(status)
}

/// The tag set `value` carries: a list of `{ Key, Value }` pairs.
///
/// What the owning service's OWN rule makes of the set is asked once the whole model has
/// been read ([`DesiredModel::read`]), so the set that rule judges is the one about to be
/// stored whether the model carried this property or left it out.
fn tag_set(value: &Value) -> Result<Vec<s3::ResourceTag>, NativeRefusal> {
    let Some(entries) = value.as_array() else {
        return Err(NativeRefusal::Invalid(format!(
            "{TAGS} must be a list of {{ {TAG_KEY}, {TAG_VALUE} }} pairs"
        )));
    };
    let mut tags = Vec::with_capacity(entries.len());
    for entry in entries {
        let key = entry.get(TAG_KEY).and_then(Value::as_str);
        let tag_value = entry.get(TAG_VALUE).and_then(Value::as_str);
        let (Some(key), Some(tag_value)) = (key, tag_value) else {
            return Err(NativeRefusal::Invalid(format!(
                "every {TAGS} entry must carry {TAG_KEY} and {TAG_VALUE}"
            )));
        };
        tags.push(s3::ResourceTag {
            key: key.to_string(),
            value: tag_value.to_string(),
        });
    }
    Ok(tags)
}
