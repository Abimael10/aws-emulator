// SPDX-License-Identifier: CC0-1.0

//! WHEN a Kafka event source mapping may be given a SCHEMA REGISTRY to validate its records
//! against.
//!
//! `KafkaSchemaRegistryConfig` is "specific configuration settings for a Kafka schema
//! registry", reached through the Kafka event source configuration of either kind
//! (`SelfManagedKafkaEventSourceConfig$SchemaRegistryConfig` and
//! `AmazonManagedKafkaEventSourceConfig$SchemaRegistryConfig`). Validating a record against a
//! registry is work the mapping's own event POLLERS do — each record is fetched, validated and
//! "delivered to your function after schema validation" by them — and that work is only done
//! by the pollers PROVISIONED MODE provides: "Use Provisioned Mode to customize the minimum
//! and maximum number of event pollers for your event source" (`ProvisionedPollerConfig`),
//! whose `MinimumPollers` is what a request turns that mode on with.
//!
//! So the two calls that state a mapping's event source configuration
//! ([`crate::ops::create_event_source_mapping`], [`crate::ops::update_event_source_mapping`])
//! admit a registry configuration only for a mapping in that mode, and refuse it with the
//! modelled `InvalidParameterValueException` otherwise, before anything is stored or changed,
//! rather than keeping a registry no poller would call. It is the same requirement the one
//! destination kind the pollers themselves produce to is held to
//! ([`super::event_source_mapping_destination`]).
//!
//! A registry those pollers DO call has to say what to validate, too. Every member of the
//! structure is optional in the MODEL, because one structure serves both the Glue and the
//! Confluent registries, but `SchemaValidationConfigs` is "an array of schema validation
//! configuration objects, which tell Lambda the message attributes you want to validate and
//! filter using your schema registry": a configuration carrying none names no attribute to
//! validate, so there is nothing for the registry to be called about, and it is refused with
//! that same modelled error ([`required_registry_field_message`]).
//!
//! A READ of the request and of the stored mapping: it decides, and changes nothing.

use crate::ops::types::{
    AmazonManagedKafkaEventSourceConfig, KafkaSchemaRegistryConfig, ProvisionedPollerConfig,
    SelfManagedKafkaEventSourceConfig,
};

/// The sentence a request stating a schema registry configuration for a mapping that is not
/// in provisioned mode is refused with.
///
/// Entirely FIXED text — it names neither the registry nor the mapping, so a caller compares
/// it exactly — and it states both the rule the request broke and the member that satisfies
/// it, which is the only way a caller learns what to send instead. RECORDED from AWS's own
/// answer to a create carrying a `SchemaRegistryConfig` and no `ProvisionedPollerConfig` (the
/// corpus unit `lambda__secretsmanager.kafka-esm-schema-registry-config-invalid-shape-rejected`).
pub const SCHEMA_REGISTRY_WITHOUT_PROVISIONED_MODE_MESSAGE: &str =
    "SchemaRegistryConfig is only available for Provisioned Mode. To configure Schema \
     Registry, please enable Provisioned Mode by specifying MinimumPollers in \
     ProvisionedPollerConfig.";

/// The Smithy member name of the registry configuration's own member that says WHAT to
/// validate, which the refusal for leaving it out names it by.
pub const SCHEMA_VALIDATION_CONFIGS_MEMBER: &str = "SchemaValidationConfigs";

/// The sentence a request whose schema registry configuration leaves out a member that
/// configuration requires is refused with, for the member `member`.
///
/// The fixed text states the requirement and the structure it is a requirement of; the
/// variable part is the member's own name, as the model declares it. RECORDED from AWS's own
/// answer to a create whose `SchemaRegistryConfig` carried no `SchemaValidationConfigs` (the
/// corpus unit `lambda__secretsmanager.kafka-esm-schema-registry-config-invalid-shape-rejected`).
#[must_use]
pub fn required_registry_field_message(member: &str) -> String {
    format!("{member} is a required field for SchemaRegistryConfig.")
}

/// The refusal for a request that states a Kafka schema registry configuration the mapping
/// could not validate anything with — and `None` when neither Kafka event source
/// configuration states a registry at all.
///
/// Two things are decided, in this order: the mapping has to be in PROVISIONED MODE for a
/// registry to be called at all, and a registry it does call has to say WHAT to validate (see
/// the module header).
///
/// Both carriers are judged, because the structure inside them is the same one and is
/// validated by the same pollers wherever it arrives.
///
/// `provisioned_poller_config` is the configuration in force AFTER this call — what the
/// request states, or what the mapping already holds for a request that states none — because
/// that is the mode the mapping would validate in, exactly as the pair `BatchSize`'s own
/// related setting binds is judged after the call ([`super::queue_batch_window_refusal`]).
#[must_use]
pub fn schema_registry_config_refusal(
    self_managed: Option<&SelfManagedKafkaEventSourceConfig>,
    amazon_managed: Option<&AmazonManagedKafkaEventSourceConfig>,
    provisioned_poller_config: Option<&ProvisionedPollerConfig>,
) -> Option<String> {
    let registry = self_managed
        .and_then(|config| config.schema_registry_config.as_ref())
        .or_else(|| amazon_managed.and_then(|config| config.schema_registry_config.as_ref()))?;
    let provisioned =
        provisioned_poller_config.is_some_and(|config| config.minimum_pollers.is_some());
    if !provisioned {
        return Some(SCHEMA_REGISTRY_WITHOUT_PROVISIONED_MODE_MESSAGE.to_string());
    }
    validation_configs_unstated(registry)
        .then(|| required_registry_field_message(SCHEMA_VALIDATION_CONFIGS_MEMBER))
}

/// Whether `registry` leaves `SchemaValidationConfigs` unstated — which a configuration
/// carrying the member as an EMPTY list does too, since an empty array names no attribute to
/// validate any more than an absent one does.
fn validation_configs_unstated(registry: &KafkaSchemaRegistryConfig) -> bool {
    registry
        .schema_validation_configs
        .as_ref()
        .is_none_or(|configs| configs.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::{KafkaSchemaRegistryConfig, KafkaSchemaValidationConfig};

    /// A registry configuration is judged on the MODE the mapping would validate in, in
    /// either carrier: it stands for a mapping whose provisioned-poller configuration states
    /// `MinimumPollers`, and is refused for one that is not in provisioned mode. A request
    /// that states no registry at all is refused nothing, whatever its mode.
    #[test]
    fn a_schema_registry_configuration_takes_provisioned_mode() {
        /// Which Kafka event source configuration a row states its registry in, if either.
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Carrier {
            SelfManaged,
            AmazonManaged,
            Neither,
        }

        /// One row: where the registry configuration is stated, the provisioned-poller
        /// configuration in force after the call, and whether the pair is refused.
        struct Row<'row> {
            case: &'row str,
            carrier: Carrier,
            provisioned: Option<ProvisionedPollerConfig>,
            refused: bool,
        }

        let rows = [
            Row {
                case: "a self-managed cluster's registry for a mapping in provisioned mode",
                carrier: Carrier::SelfManaged,
                provisioned: Some(pollers(Some(1), None)),
                refused: false,
            },
            Row {
                case: "an Amazon MSK cluster's registry for a mapping stating both bounds",
                carrier: Carrier::AmazonManaged,
                provisioned: Some(pollers(Some(2), Some(20))),
                refused: false,
            },
            Row {
                case: "a self-managed cluster's registry with no poller configuration",
                carrier: Carrier::SelfManaged,
                provisioned: None,
                refused: true,
            },
            Row {
                case: "an Amazon MSK cluster's registry with no poller configuration",
                carrier: Carrier::AmazonManaged,
                provisioned: None,
                refused: true,
            },
            Row {
                case: "a registry for a configuration that states only the maximum",
                carrier: Carrier::SelfManaged,
                provisioned: Some(pollers(None, Some(20))),
                refused: true,
            },
            Row {
                case: "no registry at all, outside provisioned mode",
                carrier: Carrier::Neither,
                provisioned: None,
                refused: false,
            },
            Row {
                case: "no registry at all, in provisioned mode",
                carrier: Carrier::Neither,
                provisioned: Some(pollers(Some(1), None)),
                refused: false,
            },
        ];

        for row in rows {
            let case = row.case;
            let registry = validating_registry();
            let self_managed = (row.carrier == Carrier::SelfManaged).then(|| {
                SelfManagedKafkaEventSourceConfig {
                    consumer_group_id: None,
                    schema_registry_config: Some(registry.clone()),
                }
            });
            let amazon_managed = (row.carrier == Carrier::AmazonManaged).then(|| {
                AmazonManagedKafkaEventSourceConfig {
                    consumer_group_id: None,
                    schema_registry_config: Some(registry.clone()),
                }
            });
            let message = schema_registry_config_refusal(
                self_managed.as_ref(),
                amazon_managed.as_ref(),
                row.provisioned.as_ref(),
            );
            let expected = row
                .refused
                .then_some(SCHEMA_REGISTRY_WITHOUT_PROVISIONED_MODE_MESSAGE);
            assert_eq!(
                message.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
    }

    /// A Kafka event source configuration that carries no registry is refused nothing, even
    /// when it states its consumer group and the mapping is not in provisioned mode.
    #[test]
    fn a_configuration_without_a_registry_is_refused_nothing() {
        let stated = SelfManagedKafkaEventSourceConfig {
            consumer_group_id: Some("orders".to_string()),
            schema_registry_config: None,
        };
        let message = schema_registry_config_refusal(Some(&stated), None, None);
        assert_eq!(
            message, None,
            "a configuration with no registry must be refused nothing, got {message:?}"
        );
    }

    /// RULE: a registry configuration the pollers DO call has to say what to validate, so one
    /// carrying no `SchemaValidationConfigs` — absent, or an empty array — is refused with
    /// that member named, while one carrying an attribute to validate stands.
    #[test]
    fn a_schema_registry_configuration_says_what_to_validate() {
        // (case, the validation configurations stated, whether the pair is refused)
        let rows: [(&str, Option<Vec<KafkaSchemaValidationConfig>>, bool); 4] = [
            (
                "one attribute to validate",
                Some(vec![validation("VALUE")]),
                false,
            ),
            (
                "both attributes to validate",
                Some(vec![validation("KEY"), validation("VALUE")]),
                false,
            ),
            ("no member at all", None, true),
            ("the member as an empty array", Some(Vec::new()), true),
        ];

        for (case, configs, refused) in rows {
            let mut registry = validating_registry();
            registry.schema_validation_configs = configs;
            let stated = SelfManagedKafkaEventSourceConfig {
                consumer_group_id: None,
                schema_registry_config: Some(registry),
            };
            let message = schema_registry_config_refusal(
                Some(&stated),
                None,
                Some(&pollers(Some(1), None)),
            );
            let expected = refused
                .then(|| required_registry_field_message(SCHEMA_VALIDATION_CONFIGS_MEMBER));
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
    }

    /// A `ProvisionedPollerConfig` stating the poller bounds given.
    fn pollers(minimum: Option<i32>, maximum: Option<i32>) -> ProvisionedPollerConfig {
        ProvisionedPollerConfig {
            minimum_pollers: minimum,
            maximum_pollers: maximum,
            poller_group_name: None,
        }
    }

    /// A registry configuration that states everything the rules above hold one to: the
    /// registry it names, the format it delivers in, and one attribute to validate.
    fn validating_registry() -> KafkaSchemaRegistryConfig {
        KafkaSchemaRegistryConfig {
            schema_registry_uri: Some(
                "arn:aws:glue:us-east-1:000000000000:registry/orders".to_string(),
            ),
            event_record_format: Some("JSON".to_string()),
            access_configs: None,
            schema_validation_configs: Some(vec![validation("VALUE")]),
        }
    }

    /// One `KafkaSchemaValidationConfig` naming the attribute given.
    fn validation(attribute: &str) -> KafkaSchemaValidationConfig {
        KafkaSchemaValidationConfig {
            attribute: Some(attribute.to_string()),
        }
    }
}
