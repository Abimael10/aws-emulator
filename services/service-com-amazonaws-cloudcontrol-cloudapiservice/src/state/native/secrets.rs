// SPDX-License-Identifier: CC0-1.0

//! `AWS::SecretsManager::Secret`, as it lives in the AWS Secrets Manager service's own store.
//!
//! A secret is a resource of the Secrets Manager service: `secretsmanager:CreateSecret`
//! creates it, `secretsmanager:DescribeSecret` and `secretsmanager:GetSecretValue` report it,
//! `secretsmanager:UpdateSecret` and `secretsmanager:TagResource` change it and
//! `secretsmanager:DeleteSecret` removes it, and the record of it lives in that service's
//! state in the same shared engine. So, exactly as for a parameter ([`super::parameters`]) or
//! a managed policy ([`super::managed_policies`]), this provider holds a handle to THAT store
//! and nothing of its own: a secret this emulator holds is one Cloud Control API resolves
//! whichever API created it — a `secretsmanager:CreateSecret` or a CloudFormation stack that
//! declared one — and a secret Secrets Manager no longer holds is one Cloud Control API
//! reports as not found.
//!
//! Secrets are regional — the owning store holds one namespace per Region — so every answer
//! here is about the Region the call was made in.
//!
//! ## The identifier is the service's
//!
//! The resource model is the type's registry schema, whose `primaryIdentifier` is
//! `/properties/Id`: the secret's own ARN, which the schema therefore also declares
//! `readOnlyProperties`. That ARN ends in six characters Secrets Manager draws per secret
//! ([`sm::new_arn_suffix`]), so no naming rule can compute it — the identifier is the owning
//! service's to assign ([`SecretProvider::assigned_identifier`]), built with that service's
//! own ARN builder, and it is what every answer of this service names the resource by.
//!
//! ## What this path applies
//!
//! Catalog source: the `AWS::SecretsManager::Secret` resource provider schema's `properties`.
//!
//! * `Id`, the secret's ARN, the type's primary identifier and its one read-only property,
//!   reported by every read;
//! * `Name`, the secret's own name, which the schema declares create-only and which the ARN
//!   is built from; a model naming none is a resource whose name the provider picks, the same
//!   way a template that declares none leaves the name to CloudFormation;
//! * `Description`, applied to the secret as `CreateSecret`'s and `UpdateSecret`'s member of
//!   the same name;
//! * `SecretString`, the secret's value, which the schema declares `writeOnlyProperties`: it
//!   is applied as the version a write stakes `AWSCURRENT` on, and the model this API REPORTS
//!   carries it nowhere ([`crate::state::reported_model`]) — the owning service's own
//!   `GetSecretValue` is what answers a secret's value;
//! * `Tags`, the `[{ Key, Value }]` list every type publishes its tag set as.
//!
//! The schema's remaining properties are ones THIS path cannot apply, so a model that asks for
//! one is refused rather than accepted and dropped — a property a caller sets has to be one
//! the owning service then holds:
//!
//! * `GenerateSecretString`, because the value it describes is one the owning service
//!   GENERATES (its own `GetRandomPassword`, with the generation parameters that property
//!   publishes), and this path, applying the work the clock owes under the store's guard, runs
//!   no generation of its own;
//! * `ReplicaRegions`, because a replica "is a separate secret in the Region" it was
//!   replicated to, created and settled by a call of the owning service's own
//!   (`ReplicateSecretToRegions`) whose lifecycle this path does not drive;
//! * `KmsKeyId`, because storing a value under a customer managed key means encrypting it
//!   under that key, which is a call on AWS KMS that this path does not make. A model may
//!   still CARRY it with the key the secret already holds — a read of a secret on such a key
//!   reports it, so a model assembled from there carries it — which is what lets an update
//!   patch something else on such a secret.

use ::std::collections::BTreeMap;
use ::std::time::SystemTime;

use ::serde_json::{Map, Value};
use ::service_com_amazonaws_secretsmanager_secretsmanager::state as sm;

use super::{
    generated_name, require_held, require_value, tag_list, tag_set, text, unsupported,
    NativeRefusal, NativeResourceProvider, SecretsManagerState,
};

/// The `TypeName` this provider stands behind.
pub const TYPE_NAME: &str = "AWS::SecretsManager::Secret";

/// The property that carries the type's primary identifier, which the schema's
/// `primaryIdentifier` points at (`/properties/Id`): the secret's ARN, which Secrets Manager
/// assigns rather than the caller naming it.
const IDENTIFIER_PROPERTY: &str = "Id";

/// The secret's own name (`Name`), which the schema declares create-only and which the ARN is
/// built from.
const NAME: &str = "Name";

/// The secret's description (`Description`).
const DESCRIPTION: &str = "Description";

/// The secret's value (`SecretString`), which the schema declares write-only.
const SECRET_STRING: &str = "SecretString";

/// The property asking the owning service to GENERATE the value (`GenerateSecretString`),
/// which this path does not apply — see this module's header.
const GENERATE_SECRET_STRING: &str = "GenerateSecretString";

/// The property declaring the Regions the secret is replicated to (`ReplicaRegions`), which
/// this path does not apply — see this module's header.
const REPLICA_REGIONS: &str = "ReplicaRegions";

/// The customer managed key the secret's values are sealed under (`KmsKeyId`), which this path
/// does not apply and a model may carry only with the key the secret already holds.
const KMS_KEY_ID: &str = "KmsKeyId";

/// The longest a secret's name may be: the domain the OWNING service's model gives `NameType`
/// (`@length 1..512`), so nothing is created here under a name that service's own API would
/// refuse.
const MAX_NAME_CHARACTERS: usize = 512;

/// The longest a secret's value may be, and the shortest: the domain that model gives
/// `SecretStringType` (`@length 1..65536`).
const MAX_VALUE_CHARACTERS: usize = 65_536;

/// The longest a secret's description may be: the domain that model gives `DescriptionType`
/// (`@length 0..2048`).
const MAX_DESCRIPTION_CHARACTERS: usize = 2048;

/// The secrets of one engine, as Cloud Control API resources.
pub struct SecretProvider {
    /// The owning service's store. A handle, so every answer is read from the live store.
    state: ::engine::ServiceState<SecretsManagerState>,
}

impl SecretProvider {
    /// The provider for the Secrets Manager store THIS engine carries, or `None` when it
    /// carries none.
    pub fn for_engine<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<Self> {
        sm::service_state(engine).map(|state| Self { state })
    }
}

impl NativeResourceProvider for SecretProvider {
    fn type_name(&self) -> &'static str {
        TYPE_NAME
    }

    fn identifier_property(&self) -> &'static str {
        IDENTIFIER_PROPERTY
    }

    /// The ARN the secret a create describes will answer to.
    ///
    /// The type's primary identifier is read-only in its schema, so no desired state carries
    /// it: the ARN is the owning service's to assign, and it assigns the one built from the
    /// partition, this account, the Region, the secret's name and the six characters it draws
    /// per secret — which is exactly what that service's own create allocates
    /// ([`sm::authorization::created_secret_arn`]): the ARN a name the Region ALREADY holds a
    /// secret under, so a second create meets that service's own already-exists verdict rather
    /// than being recorded against an ARN nothing answers to, else a fresh one.
    ///
    /// A model that names no name is a resource whose name the provider picks
    /// ([`generated_name`]), the same way a template that declares none leaves the name to
    /// CloudFormation.
    fn assigned_identifier(&self, region: &str, properties: &Value) -> Option<String> {
        let name = properties
            .get(NAME)
            .and_then(Value::as_str)
            .map(str::to_string)
            // A secret name carries no suffix any kind of it requires, so the generated one is
            // the bare minted token.
            .unwrap_or_else(|| generated_name(false, ""));
        let held = {
            let store = self.state.lock();
            store.named(region, &name).map(|secret| secret.arn.clone())
        };
        Some(held.unwrap_or_else(|| {
            sm::authorization::secret_arn(region, &format!("{name}-{}", sm::new_arn_suffix()))
        }))
    }

    fn read(&self, region: &str, identifier: &str) -> Option<Value> {
        let mut store = self.state.lock();
        // What the clock already owes the owning store: a secret whose permanent removal has
        // fallen due is one that service would already have dropped, whether or not anybody
        // called, so it is not a secret to report here either.
        store.expire_due(SystemTime::now());
        held(&store, region, identifier).map(model)
    }

    fn list(&self, region: &str, after: Option<&str>, limit: usize) -> Vec<(String, Value)> {
        let mut store = self.state.lock();
        store.expire_due(SystemTime::now());
        // The owning store keys the Region's secrets by NAME, so the ARN order a listing pages
        // over is built here — over the records that sort after the position the page resumes
        // at, which is bounded by that store's own secret quota.
        let mut found: Vec<(String, Value)> = store
            .in_region(region)
            .filter(|secret| after.is_none_or(|after| secret.arn.as_str() > after))
            .map(|secret| (secret.arn.clone(), model(secret)))
            .collect();
        found.sort_by(|(left, _), (right, _)| left.cmp(right));
        found.truncate(limit);
        found
    }

    fn create(
        &self,
        region: &str,
        identifier: &str,
        properties: &Value,
        at: SystemTime,
    ) -> Result<Value, NativeRefusal> {
        let name = named(region, identifier)?;
        let desired = DesiredModel::read(
            properties,
            &Held {
                arn: identifier,
                name: &name,
                // A create seals no value under a customer managed key, so the secret it makes
                // holds none and a model naming one asks for work this path does not do.
                kms_key_id: None,
            },
        )?;
        let value = desired.value()?;

        let mut store = self.state.lock();
        store.expire_due(at);
        // A secret name belongs to one secret in one Region, so a second create under a name
        // this Region already holds is the owning service's own already-exists verdict.
        if store.named(region, &name).is_some() {
            return Err(NativeRefusal::AlreadyExists);
        }
        if store.secrets.len() >= sm::SECRET_QUOTA {
            return Err(NativeRefusal::AtCapacity(format!(
                "The request failed because it would exceed the maximum of {} secrets this \
                 emulator stores.",
                sm::SECRET_QUOTA
            )));
        }
        let mut secret = sm::Secret::new(
            name.clone(),
            identifier.to_string(),
            region.to_string(),
            at,
        );
        desired.apply(&mut secret);
        // "If you include SecretString [...] then Secrets Manager creates an initial secret
        // version and automatically attaches the staging label AWSCURRENT to it" — so a secret
        // created with no value has no version at all.
        if let Some(value) = value {
            stake_current_version(&mut secret, &value, at);
        }
        let created = model(&secret);
        store.insert(secret);
        Ok(created)
    }

    fn update(
        &self,
        region: &str,
        identifier: &str,
        properties: &Value,
        at: SystemTime,
    ) -> Result<Value, NativeRefusal> {
        let mut store = self.state.lock();
        store.expire_due(at);
        let (name, kms_key_id) = {
            let secret = held(&store, region, identifier).ok_or(NativeRefusal::NotFound)?;
            (secret.name.clone(), secret.kms_key_id.clone())
        };
        let desired = DesiredModel::read(
            properties,
            &Held {
                arn: identifier,
                name: &name,
                kms_key_id: kms_key_id.as_deref(),
            },
        )?;
        let value = desired.value()?;
        let key = sm::SecretKey::new(region.to_string(), name);
        let secret = store
            .secrets
            .get_mut(&key)
            .ok_or(NativeRefusal::NotFound)?;
        desired.apply(secret);
        // "If you include SecretString [...] creates a new version" — unless the version
        // `AWSCURRENT` already points at holds exactly that value, which asks for nothing:
        // a model a client round-tripped through this API carries no value at all (the
        // property is write-only), so this is the state a repeated write of one value leaves.
        if let Some(value) = value {
            let unchanged = secret
                .version_with_stage(sm::STAGE_CURRENT)
                .is_some_and(|current| current.holds_same_value(&value));
            if !unchanged {
                stake_current_version(secret, &value, at);
            }
        }
        secret.last_changed_date = at;
        Ok(model(secret))
    }

    fn delete(&self, region: &str, identifier: &str, at: SystemTime) -> Result<(), NativeRefusal> {
        let mut store = self.state.lock();
        store.expire_due(at);
        let key = {
            let secret = held(&store, region, identifier).ok_or(NativeRefusal::NotFound)?;
            sm::SecretKey::new(secret.region.clone(), secret.name.clone())
        };
        // This type's delete removes the secret with NO recovery window — the `DeleteSecret`
        // the registry's own provider makes carries `ForceDeleteWithoutRecovery` — and the
        // short asynchronous delay the owning service takes over such a removal has already
        // elapsed by the time this work falls due ([`crate::state::RESOURCE_OPERATION_SPAN`]),
        // so the secret is gone: its name is free for a later create, and every read addressed
        // by its ARN reports no secret afterwards.
        store.remove(&key);
        Ok(())
    }
}

/// The secret `identifier` names in `region`, or `None` when the owning service holds none
/// there.
///
/// A secret is addressed by the primary identifier its schema declares — its own ARN — so the
/// lookup is the owning store's ARN index, narrowed to the Region the call was made in: an ARN
/// naming another Region's secret names no resource of this account and Region.
fn held<'a>(
    store: &'a SecretsManagerState,
    region: &str,
    identifier: &str,
) -> Option<&'a sm::Secret> {
    let key = store.arn_index.get(identifier)?;
    if key.region != region {
        return None;
    }
    store.secrets.get(key)
}

/// The resource model one secret renders as: its identity, and the properties of the type's
/// schema the owning record carries.
///
/// The write-only `SecretString` is in no model: it is applied to the secret, and the owning
/// service's own `GetSecretValue` is what answers a secret's value.
fn model(secret: &sm::Secret) -> Value {
    let mut model = Map::new();
    model.insert(
        IDENTIFIER_PROPERTY.to_string(),
        Value::from(secret.arn.clone()),
    );
    model.insert(NAME.to_string(), Value::from(secret.name.clone()));
    if let Some(description) = &secret.description {
        model.insert(DESCRIPTION.to_string(), Value::from(description.clone()));
    }
    if let Some(key) = &secret.kms_key_id {
        model.insert(KMS_KEY_ID.to_string(), Value::from(key.clone()));
    }
    let tags = tags_held(secret);
    if !tags.is_empty() {
        model.insert(super::TAGS.to_string(), tag_list(tags.iter()));
    }
    Value::Object(model)
}

/// The tag set the owning record holds, folded by key — both halves of a stored tag are
/// optional, as the owning service's modelled `Tag` has them, and a tag set reports the
/// entries that carry both.
fn tags_held(secret: &sm::Secret) -> BTreeMap<String, String> {
    secret
        .tags
        .iter()
        .filter_map(|tag| Some((tag.key.clone()?, tag.value.clone()?)))
        .collect()
}

/// Stake `value` as the secret's current version, which is what a write of a secret's value
/// does: a fresh version id, carrying `AWSCURRENT`, which the owning record moves off whichever
/// version held it ([`sm::Secret::put_version`]).
///
/// The value is held AS SENT, which is how the owning store holds the value of a secret on the
/// Amazon Web Services managed key: no customer managed key is named here, so no KMS call
/// stands between a read and the value.
fn stake_current_version(
    secret: &mut sm::Secret,
    value: &sm::envelope::SecretValue,
    at: SystemTime,
) {
    secret.put_version(
        sm::SecretVersion {
            version_id: sm::new_version_id(),
            value: sm::envelope::StoredValue::as_sent(value),
            version_stages: vec![sm::STAGE_CURRENT.to_string()],
            created_date: at,
            // Never retrieved yet: `LastAccessedDate` "is omitted if the secret has never been
            // retrieved".
            last_accessed_date: None,
        },
        at,
    );
}

/// What the resource the model is applied to already HOLDS: the values a model may carry for
/// its identity and for the properties this path does not apply.
struct Held<'a> {
    /// The secret's ARN, which is the type's primary identifier.
    arn: &'a str,
    /// The secret's name.
    name: &'a str,
    /// The customer managed key the secret's values are sealed under, and `None` for a secret
    /// on the Amazon Web Services managed key.
    kms_key_id: Option<&'a str>,
}

/// The desired state of a secret, as this path applies it.
#[derive(Default)]
struct DesiredModel {
    /// The description the model states, and `None` for a model that states none — which is a
    /// secret with no description, since a model this API reports carries the description the
    /// secret holds.
    description: Option<String>,
    /// The value the model states, and `None` for a model that states none. The property is
    /// write-only, so no model this API reports carries it and a model that omits it leaves the
    /// value the secret holds exactly where it is.
    value: Option<String>,
    /// The tag set the model states, and `None` for a model that states none — which is a
    /// secret with no tags.
    tags: Option<BTreeMap<String, String>>,
}

impl DesiredModel {
    /// The desired state `properties` describes for the resource `held` describes, or why it
    /// is not one.
    fn read(properties: &Value, held: &Held<'_>) -> Result<Self, NativeRefusal> {
        let Some(model) = properties.as_object() else {
            return Err(NativeRefusal::Invalid(format!(
                "{TYPE_NAME} requires a resource model of its properties"
            )));
        };
        let mut desired = Self::default();
        for (property, value) in model {
            match property.as_str() {
                IDENTIFIER_PROPERTY => require_value(property, value, held.arn)?,
                NAME => require_value(property, value, held.name)?,
                DESCRIPTION => desired.description = Some(text(property, value)?),
                SECRET_STRING => desired.value = Some(text(property, value)?),
                KMS_KEY_ID => require_held(TYPE_NAME, property, value, held.kms_key_id)?,
                TAGS_PROPERTY => desired.tags = Some(tag_set(value)?),
                GENERATE_SECRET_STRING | REPLICA_REGIONS => {
                    return Err(unsupported(TYPE_NAME, property));
                }
                other => {
                    return Err(NativeRefusal::Invalid(format!(
                        "{other} is not a property of {TYPE_NAME}"
                    )));
                }
            }
        }
        if desired
            .description
            .as_ref()
            .is_some_and(|text| text.chars().count() > MAX_DESCRIPTION_CHARACTERS)
        {
            return Err(NativeRefusal::Invalid(format!(
                "{DESCRIPTION} must be at most {MAX_DESCRIPTION_CHARACTERS} characters"
            )));
        }
        if desired
            .tags
            .as_ref()
            .is_some_and(|tags| tags.len() > sm::TAGS_PER_SECRET)
        {
            return Err(NativeRefusal::AtCapacity(format!(
                "A secret carries at most {} tags.",
                sm::TAGS_PER_SECRET
            )));
        }
        Ok(desired)
    }

    /// The value this model states, as the owning service reads one from a write — or why it
    /// is not a value that service would store.
    fn value(&self) -> Result<Option<sm::envelope::SecretValue>, NativeRefusal> {
        let Some(stated) = self.value.as_ref() else {
            return Ok(None);
        };
        if stated.is_empty() || stated.chars().count() > MAX_VALUE_CHARACTERS {
            return Err(NativeRefusal::Invalid(format!(
                "{SECRET_STRING} must be 1 to {MAX_VALUE_CHARACTERS} characters"
            )));
        }
        Ok(sm::envelope::SecretValue::from_request(Some(stated), None))
    }

    /// Apply everything but the value to `secret`: the description and the tag set, each as the
    /// model states it — so a model that states neither leaves the secret carrying neither,
    /// which is what a desired state the property is absent from describes.
    fn apply(&self, secret: &mut sm::Secret) {
        // `DescriptionType` is modelled with a minimum length of 0, so the empty string is a
        // description of nothing: the secret carries none, exactly as one created without the
        // member does.
        secret.description = self
            .description
            .clone()
            .filter(|text| !text.is_empty());
        secret.tags = self
            .tags
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|(key, value)| sm::StoredTag {
                key: Some(key),
                value: Some(value),
            })
            .collect();
    }
}

/// The property a tag set travels under, as the registry spells it for every type.
const TAGS_PROPERTY: &str = super::TAGS;

/// The NAME of the secret the ARN `identifier` names, or why that ARN names no secret of this
/// account and Region.
///
/// The identifier a create is recorded under is the ARN this provider assigned, and it is
/// checked back against the OWNING service's own ARN builder: the prefix is the one that
/// builder produces for this Region, and the resource is a name plus the six characters a
/// secret's ARN ends in ([`sm::secret_name_in`] is the reader that takes them off). A name
/// outside what that service accepts is one its own API would refuse.
fn named(region: &str, identifier: &str) -> Result<String, NativeRefusal> {
    let prefix = sm::authorization::secret_arn(region, "");
    let Some(resource) = identifier.strip_prefix(&prefix) else {
        return Err(arn_requirement());
    };
    let name = sm::secret_name_in(identifier);
    if resource.len() != name.len() + 1 + sm::ARN_SUFFIX_LEN {
        return Err(arn_requirement());
    }
    if !sm::is_valid_secret_name(name) || name.chars().count() > MAX_NAME_CHARACTERS {
        return Err(NativeRefusal::Invalid(
            sm::INVALID_SECRET_NAME_MESSAGE.to_string(),
        ));
    }
    Ok(name.to_string())
}

/// What the identifier of a resource of this type has to be, stated for a value that is not
/// one.
fn arn_requirement() -> NativeRefusal {
    NativeRefusal::Invalid(format!(
        "the primary identifier of {TYPE_NAME} is the ARN Secrets Manager assigned the secret"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the identifier a create is recorded under is an ARN the OWNING service's own
    /// builder produces for a secret of this Region, and the name it carries is the secret's.
    ///
    /// The rows differ along what the rule generalises over: the form of the name (plain, the
    /// punctuation a secret name admits, hierarchical, one that itself ends in a hyphen and six
    /// characters), and the ways an identifier can fail to be such an ARN.
    #[test]
    fn the_identifier_is_an_arn_of_this_regions_secret_namespace() {
        let region = "us-east-1";
        // (case, the name the ARN was built for, the name it must read back as).
        let accepted: &[(&str, &str)] = &[
            ("orders", "orders"),
            ("prod/db/password_v2+1=a.b@c-d", "prod/db/password_v2+1=a.b@c-d"),
            ("orders-ABC123", "orders-ABC123"),
            (&"n".repeat(MAX_NAME_CHARACTERS), &"n".repeat(MAX_NAME_CHARACTERS)),
        ];
        for (built_for, expected) in accepted {
            let arn = sm::authorization::secret_arn(region, &format!("{built_for}-AbCd12"));
            let read = named(region, &arn);
            assert_eq!(
                read.as_deref(),
                Ok(*expected),
                "case {built_for:?}: expected the name {expected:?}, got {read:?}"
            );
        }

        let refused: &[(&str, String)] = &[
            (
                "an ARN of another service",
                ::engine::emulator_arn("ssm", region, "parameter/orders"),
            ),
            (
                "an ARN of another Region's secret namespace",
                sm::authorization::secret_arn("eu-west-1", "orders-AbCd12"),
            ),
            (
                "an ARN carrying no six characters of its own",
                sm::authorization::secret_arn(region, "orders"),
            ),
            (
                "a name the owning service would refuse",
                sm::authorization::secret_arn(region, "not a name-AbCd12"),
            ),
            (
                "a name past the modelled length",
                sm::authorization::secret_arn(
                    region,
                    &format!("{}-AbCd12", "n".repeat(MAX_NAME_CHARACTERS + 1)),
                ),
            ),
            ("a bare name", "orders".to_string()),
        ];
        for (case, identifier) in refused {
            let read = named(region, identifier);
            assert!(
                read.is_err(),
                "case {case:?}: expected a refusal of {identifier:?}, got {read:?}"
            );
        }
    }
}
