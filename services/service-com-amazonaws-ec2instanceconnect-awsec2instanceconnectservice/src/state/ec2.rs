// SPDX-License-Identifier: CC0-1.0

//! The hop into Amazon EC2: the owner of every instance a key push names.
//!
//! An EC2 instance is EC2's resource, not this service's — this service models no operation
//! that launches, stops or terminates one — so whether an instance exists, which
//! Availability Zone it was launched in, what lifecycle state it is in, which instance
//! type it runs and which tags it carries is EC2's answer to give. Both pushes therefore ASK
//! EC2, over the AWS SDK
//! driven on the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), which routes inside this
//! emulator: no socket is opened, no name is resolved, nothing leaves the process, and this
//! crate names no other service's crate.
//!
//! What EC2 answers decides the push: an instance it reports is the instance the push is
//! served against, and an id it does not hold is `EC2InstanceNotFoundException`. Only when
//! EC2 cannot be asked AT ALL — an emulator with no EC2 mounted, where no service can
//! answer for an instance — is nothing known
//! ([`InstanceLookup::Unanswered`]), and the caller falls back to this service's own
//! registry ([`super::seed_instances`]) rather than inventing an answer here. Applying that
//! rule is [`InstanceLookup::resolve`], which is where both pushes resolve the instance a
//! key is pushed at.

use std::collections::BTreeMap;

use super::{
    ComAmazonawsEc2instanceconnectAwsec2instanceconnectserviceState as ServiceStore, Instance,
    InstanceState,
};

/// What EC2 said about one instance id.
#[derive(Debug, Clone)]
pub enum InstanceLookup {
    /// EC2 holds an instance with that id: this much of the record it reported.
    Owned(Instance),
    /// EC2 answered, and holds no instance with that id.
    Absent,
    /// EC2 could not be asked at all, so nothing is known about the id.
    Unanswered,
}

impl InstanceLookup {
    /// The record a push is served against, or `None` for an id whose owner holds no
    /// instance.
    ///
    /// This is the fallback rule, in one place for both pushes: EC2's answer decides — an
    /// instance it reported IS the instance the push is served against, and an id it
    /// answered "absent" for is not found whatever this service's own registry holds — and
    /// the fallback registry ([`super::seed_instances`]) is read ONLY when EC2 could not be
    /// asked at all. Both operations map the `None` to their own
    /// `EC2InstanceNotFoundException`, so the rule lives here rather than once per handler,
    /// the way the state-to-outcome rule lives on [`InstanceState::blocks_connect`].
    ///
    /// `fallback` is this service's store ([`super::service_state`]); it is locked only on
    /// the branch that reads it, so the registry is never touched while EC2's answer stands.
    pub fn resolve(
        self,
        instance_id: &str,
        fallback: &::engine::ServiceState<ServiceStore>,
    ) -> Option<Instance> {
        match self {
            Self::Owned(instance) => Some(instance),
            Self::Absent => None,
            Self::Unanswered => fallback.lock().instance(instance_id).cloned(),
        }
    }
}

/// The `InstanceTypeHypervisor` value EC2 reports for the instance types the serial console
/// supports: "Only Nitro instance types are currently supported"
/// (`EC2InstanceTypeInvalidException`), and whether a type runs on the Nitro System is a
/// fact of EC2's own instance-type catalog (`DescribeInstanceTypes`,
/// `InstanceTypeInfo$Hypervisor`) rather than a list kept here.
const NITRO_HYPERVISOR: &str = "nitro";

/// The EC2 client every hop here goes through.
///
/// `region` is the request's own signing region, so the hop stays in the Region the caller
/// addressed. The client is given the callback's own HTTP client, so the endpoint URL below
/// is only the address that transport routes on IN PROCESS. The credentials are the
/// callback's, which sign this in-process hop and never appear in a response.
fn client(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_ec2::Client {
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_ec2::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://ec2.{region}.amazonaws.com"))
        // The transport is in-process: there is nothing to retry, time out or watch for a
        // stalled stream, and each of those would need a sleep implementation this client is
        // not given. The same goes for caching the identity: the credentials the callback
        // hands over are used directly.
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_ec2::Client::from_conf(config)
}

/// Ask EC2 about `instance_id` and answer what it said.
///
/// The record is built from the members EC2 reports for the instance — its state
/// (`Instance$State`), its Availability Zone (`Instance$Placement`), its instance type
/// (`Instance$InstanceType`) and its tags (`Instance$Tags`) — plus whether that type runs on
/// the Nitro System, which is a second question only EC2's instance-type catalog can answer
/// ([`instance_type_is_nitro`]).
pub async fn instance(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    instance_id: &str,
) -> InstanceLookup {
    let client = client(ctx, region);
    let answer = client
        .describe_instances()
        .instance_ids(instance_id)
        .send()
        .await;
    let described = match answer {
        Ok(output) => output,
        Err(error) => {
            // EC2 refusing the id — an id it does not hold, or one it reads as malformed —
            // IS an answer: it holds no such instance. Anything else means the hop never
            // reached a service that could answer for an instance.
            return match refusal_code(&error).as_deref() {
                Some("InvalidInstanceID.NotFound") | Some("InvalidInstanceID.Malformed") => {
                    InstanceLookup::Absent
                }
                _ => {
                    ::tracing::debug!(
                        %region,
                        %instance_id,
                        error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                        "Amazon EC2 could not be asked about the instance a key push named"
                    );
                    InstanceLookup::Unanswered
                }
            };
        }
    };

    let Some(reported) = described
        .reservations()
        .iter()
        .flat_map(|reservation| reservation.instances())
        .find(|reported| reported.instance_id() == Some(instance_id))
    else {
        return InstanceLookup::Absent;
    };

    // The lifecycle state decides every other answer, so a value this service cannot read
    // establishes nothing rather than standing in for a state EC2 did not name.
    let Some(state) = reported
        .state()
        .and_then(|state| state.name())
        .and_then(|name| InstanceState::from_ec2_name(name.as_str()))
    else {
        ::tracing::debug!(
            %region,
            %instance_id,
            "Amazon EC2 reported the instance of a key push in no lifecycle state this \
             service knows"
        );
        return InstanceLookup::Unanswered;
    };

    let instance_type = reported
        .instance_type()
        .map(|instance_type| instance_type.as_str().to_string())
        .unwrap_or_default();
    let serial_console_supported = instance_type_is_nitro(ctx, region, &instance_type)
        .await
        .unwrap_or(false);

    // The instance's tags are EC2's too (`Instance$Tags`), and a key push is authorized over
    // them, so the set EC2 reported is carried back — as a set that is KNOWN only where EC2
    // reported it in full ([`known_tags`]).
    let tags = known_tags(reported.tags());

    InstanceLookup::Owned(Instance {
        instance_id: instance_id.to_string(),
        availability_zone: reported
            .placement()
            .and_then(|placement| placement.availability_zone())
            .unwrap_or_default()
            .to_string(),
        instance_type,
        state,
        serial_console_supported,
        tags,
    })
}

/// The tag set EC2 reported for an instance, where it reported it IN FULL, and `None` where it
/// did not.
///
/// A key push is authorized over the tags of the instance it names, and publishing the tag
/// condition keys for a set states, as a fact, that every key absent from it is a tag the
/// instance does not carry ([`super::authorization::Conditions::new`]) — which only a set read
/// in full supports. A tag EC2 reports without a key or without a value is not a
/// `(key, value)` pair an authorization clause can be decided against, and inventing an empty
/// string for the missing half would state a pair EC2 did not report, so a reported set
/// carrying one is not known: none of it is published, rather than a set with that tag
/// dropped being passed off as the complete one.
fn known_tags(reported: &[::aws_sdk_ec2::types::Tag]) -> Option<BTreeMap<String, String>> {
    reported
        .iter()
        .map(|tag| Some((tag.key()?.to_string(), tag.value()?.to_string())))
        .collect()
}

/// Whether `instance_type` runs on the AWS Nitro System, as EC2's instance-type catalog
/// reports it (`InstanceTypeInfo$Hypervisor`); `None` when EC2 did not answer, so the
/// capability is not established.
pub async fn instance_type_is_nitro(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    instance_type: &str,
) -> Option<bool> {
    if instance_type.is_empty() {
        return None;
    }
    let described = client(ctx, region)
        .describe_instance_types()
        .instance_types(::aws_sdk_ec2::types::InstanceType::from(instance_type))
        .send()
        .await
        .ok()?;
    let hypervisor = described
        .instance_types()
        .iter()
        .find(|info| {
            info.instance_type()
                .is_some_and(|name| name.as_str() == instance_type)
        })
        .and_then(|info| info.hypervisor())?;
    Some(hypervisor.as_str() == NITRO_HYPERVISOR)
}

/// Whether the account may use the EC2 serial console, as EC2 reports it
/// (`GetSerialConsoleAccessStatus`, the status EC2's own `EnableSerialConsoleAccess` and
/// `DisableSerialConsoleAccess` move); `None` when EC2 did not answer, so the caller falls
/// back to the toggle this service keeps.
pub async fn serial_console_access_enabled(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<bool> {
    client(ctx, region)
        .get_serial_console_access_status()
        .send()
        .await
        .ok()?
        .serial_console_access_enabled()
}

/// The error code EC2 answered a refusal with, when the failure carries one at all.
fn refusal_code<E, R>(error: &::aws_sdk_ec2::error::SdkError<E, R>) -> Option<String>
where
    E: ::aws_sdk_ec2::error::ProvideErrorMetadata + ::std::error::Error + 'static,
    R: ::std::fmt::Debug,
{
    use ::aws_sdk_ec2::error::ProvideErrorMetadata;
    error.code().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One `Instance$Tags` entry as EC2 reports it, with either half left out when it is
    /// `None`.
    fn tag(key: Option<&str>, value: Option<&str>) -> ::aws_sdk_ec2::types::Tag {
        let mut builder = ::aws_sdk_ec2::types::Tag::builder();
        if let Some(key) = key {
            builder = builder.key(key);
        }
        if let Some(value) = value {
            builder = builder.value(value);
        }
        builder.build()
    }

    /// One row: what the case is called, the tags EC2 reported, and the set that is then
    /// known in full (`None` for a set that is not known).
    type Row = (
        &'static str,
        Vec<::aws_sdk_ec2::types::Tag>,
        Option<Vec<(&'static str, &'static str)>>,
    );

    /// A tag set EC2 reported in full is known, the empty set included — EC2 answered for the
    /// instance, so an empty list is the fact that it carries no tags — while a set carrying a
    /// tag either half of which EC2 did not report is not known at all, whatever else it
    /// carries.
    #[test]
    fn a_tag_set_is_known_only_where_every_tag_ec2_reported_is_a_pair() {
        let pair = ("Environment", "staging");
        let rows: Vec<Row> = vec![
            ("no tag reported at all", vec![], Some(vec![])),
            (
                "one whole pair",
                vec![tag(Some(pair.0), Some(pair.1))],
                Some(vec![pair]),
            ),
            (
                "two whole pairs",
                vec![
                    tag(Some(pair.0), Some(pair.1)),
                    tag(Some("Team"), Some("platform")),
                ],
                Some(vec![("Team", "platform"), pair]),
            ),
            (
                "a value EC2 reported as empty, which is a value",
                vec![tag(Some(pair.0), Some(""))],
                Some(vec![(pair.0, "")]),
            ),
            ("a tag with no value", vec![tag(Some(pair.0), None)], None),
            ("a tag with no key", vec![tag(None, Some(pair.1))], None),
            (
                "one whole pair beside a tag with no value",
                vec![tag(Some(pair.0), Some(pair.1)), tag(Some("Team"), None)],
                None,
            ),
        ];
        for (case, reported, expected) in rows {
            let expected = expected.map(|pairs| {
                pairs
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect::<BTreeMap<String, String>>()
            });
            let known = known_tags(&reported);
            assert_eq!(
                known, expected,
                "case {case:?}: expected {expected:?}, got {known:?}"
            );
        }
    }
}
