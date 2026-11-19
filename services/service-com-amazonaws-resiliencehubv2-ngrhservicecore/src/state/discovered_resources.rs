// SPDX-License-Identifier: CC0-1.0

//! Which resources a SERVICE has discovered — asked of the component that owns each one.
//!
//! A service's resources are not supplied by a caller: they are DISCOVERED through the
//! service's input sources (`Resource` is "an AWS resource discovered by Resilience Hub",
//! and `ServiceResource` reports "the input source that discovered the resource"). So the
//! set of resources a service holds is a reading of those input sources, taken afresh on
//! every call rather than cached on a record: a stack that gained a resource after the
//! input source was created has that resource discovered too, with no further call to this
//! service.
//!
//! Only one input-source kind names resources a component of this emulator OWNS: a
//! `cfnStackArn` source, whose resources are that stack's own, read through
//! CloudFormation's `DescribeStackResources` over the emulator's in-process callback
//! ([`smithy_server_runtime::EmbeddedEmulator`]) — the endpoint url names CloudFormation's
//! public host so the request is signed and routed as that service's, but no socket is
//! opened and no name is resolved, so a resource is reached only where this emulator
//! serves it itself. A resource is therefore reported as discovered only if the service
//! that holds it returns it from its own read.
//!
//! The other kinds (`resourceTags`, `tfStateFileUrl`, `eks`, `designFileS3Url`) name
//! resources nothing here holds — an account-wide tag query, a Terraform state file, the
//! workloads of an EKS cluster, a design document — so this emulator cannot say what they
//! discover. A service carrying one of them is UNANSWERED
//! ([`Discovery::every_source_answered`]) and nothing about its resources is refused on
//! the strength of a question nobody answered; a service whose every source was answered
//! for is judged exactly, which includes a service with no input source at all, since
//! nothing then discovers anything.

use ::smithy_server_runtime::EmbeddedEmulator;

use crate::ops::types::ResourceConfiguration;

use super::{ComAmazonawsResiliencehubv2NgrhservicecoreState, INPUT_SOURCE_TYPE_CFN_STACK};

/// CloudFormation's ARN namespace: the vendor field a `cfnStackArn` carries, and the host
/// the in-process read addresses.
const CLOUDFORMATION: &str = "cloudformation";

/// The error code CloudFormation answers for a stack it does not hold ("Stack with id
/// <name> does not exist"), which is a source that discovers NOTHING rather than
/// CloudFormation failing to answer.
const STACK_DOES_NOT_EXIST_CODE: &str = "ValidationError";

/// The resource status CloudFormation reports for a resource a stack no longer holds. It
/// keeps reporting the row for as long as it reports the stack, so such a row is not a
/// resource that exists to be discovered.
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";

/// The ceiling on how many resources one service function will hold.
///
/// The modelled `ResourceList` is `len 1..100` (`smithy_query(query="shape",
/// shape="ResourceList")`) and it is the shape every reader of a function's resources
/// carries, so a function holding more than this has no representation on the wire. It is
/// also what keeps an unauthenticated caller from growing the process's memory without
/// limit by repeating `CreateServiceFunctionResources` with fresh identifiers.
pub const MAX_RESOURCES_PER_SERVICE_FUNCTION: usize = 100;

/// One of a service's input sources, as far as this emulator can read what it discovers.
#[derive(Debug, Clone)]
pub enum DiscoverySource {
    /// A CloudFormation stack (`cfnStackArn`): what it discovers is the stack's own
    /// resources, which CloudFormation holds and reports.
    CfnStack {
        input_source_id: String,
        stack_arn: String,
    },
    /// A source whose resources no component of this emulator holds, so what it discovers
    /// cannot be read here. See the module docs.
    Unreadable,
}

/// One resource a service's input source discovered, and which source discovered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredResource {
    /// The identifier the resource is addressed by — what `ResourceList` carries and what
    /// `ServiceResource$resourceIdentifier` reports.
    pub identifier: String,
    /// The resource's type as its owner reports it, absent where the owner reports none.
    pub resource_type: Option<String>,
    /// The `inputSourceId` of the source that discovered it.
    pub input_source_id: String,
    /// That source's `InputSourceType`.
    pub input_source_type: &'static str,
    /// The Region the resource lives in: the one its owning stack lives in.
    pub aws_region: String,
}

/// What a service has discovered, and whether every one of its input sources was answered
/// for.
#[derive(Debug, Clone)]
pub struct Discovery {
    /// The resources the answered sources discovered, in source order.
    pub resources: Vec<DiscoveredResource>,
    /// Whether what EVERY one of the service's input sources discovers was readable here.
    /// `false` leaves the service's resource set unknown, so nothing is refused for being
    /// outside it.
    pub every_source_answered: bool,
}

impl Discovery {
    /// Whether `identifier` is one of the resources discovered.
    pub fn holds(&self, identifier: &str) -> bool {
        self.resources
            .iter()
            .any(|resource| resource.identifier == identifier)
    }

    /// The first of `requested` this service has not discovered, where that can be said at
    /// all: `None` when every requested identifier was discovered, and for a service one of
    /// whose input sources could not be read here.
    pub fn first_undiscovered<'a>(&self, requested: &'a [String]) -> Option<&'a str> {
        if !self.every_source_answered {
            return None;
        }
        requested
            .iter()
            .map(String::as_str)
            .find(|identifier| !self.holds(identifier))
    }
}

impl ComAmazonawsResiliencehubv2NgrhservicecoreState {
    /// The input sources `service_arn` discovers its resources through, in creation order.
    ///
    /// Read under this store's guard and handed back as plain values, so the discovery that
    /// follows — which asks another component over the in-process callback — holds no store
    /// guard.
    pub fn discovery_sources(&self, service_arn: &str) -> Vec<DiscoverySource> {
        self.input_sources_for(service_arn)
            .map(|collection| {
                collection
                    .in_creation_order()
                    .into_iter()
                    .map(|source| match &source.configuration {
                        ResourceConfiguration::cfnStackArn(stack_arn) => {
                            DiscoverySource::CfnStack {
                                input_source_id: source.input_source_id.clone(),
                                stack_arn: stack_arn.clone(),
                            }
                        }
                        _ => DiscoverySource::Unreadable,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// What `sources` discover, asked of the component that owns each one's resources.
pub async fn discover(callback: &EmbeddedEmulator, sources: &[DiscoverySource]) -> Discovery {
    let mut resources: Vec<DiscoveredResource> = Vec::new();
    let mut every_source_answered = true;
    for source in sources {
        match source {
            DiscoverySource::CfnStack {
                input_source_id,
                stack_arn,
            } => match stack_resources(callback, stack_arn).await {
                Some(found) => resources.extend(found.into_iter().map(
                    |(identifier, resource_type, aws_region)| DiscoveredResource {
                        identifier,
                        resource_type,
                        input_source_id: input_source_id.clone(),
                        input_source_type: INPUT_SOURCE_TYPE_CFN_STACK,
                        aws_region,
                    },
                )),
                None => every_source_answered = false,
            },
            DiscoverySource::Unreadable => every_source_answered = false,
        }
    }
    Discovery {
        resources,
        every_source_answered,
    }
}

/// The resources CloudFormation reports for the stack `stack_arn` names, each as its
/// identifier, its type and the Region it lives in.
///
/// `None` means CloudFormation was not ASKED or did not answer — which says nothing about
/// what the source discovers — while `Some(empty)` is CloudFormation answering that there
/// is no such stack, or none of its resources exists any more.
async fn stack_resources(
    callback: &EmbeddedEmulator,
    stack_arn: &str,
) -> Option<Vec<(String, Option<String>, String)>> {
    // A stack lives in the Region its own ARN names, read under the Region grammar the
    // models state, and a stack of another account is not this emulator's to hold, so
    // nothing can be said about what such a source discovers.
    let region = ::service_common::arn::region_of(stack_arn, CLOUDFORMATION)?;
    let account = ::service_common::arn::parse(stack_arn)?.account;
    if account != ::engine::emulator_account_id() {
        return None;
    }
    let answered = cloudformation_client(callback, region)
        .describe_stack_resources()
        .stack_name(stack_arn)
        .send()
        .await;
    let described = match answered {
        Ok(described) => described,
        Err(error) => {
            // CloudFormation's OWN "no such stack" is an answer: that source discovers
            // nothing. Anything else is this emulator failing to ask, which is not the
            // same claim.
            let code = ::aws_sdk_cloudformation::error::ProvideErrorMetadata::code(&error);
            return (code == Some(STACK_DOES_NOT_EXIST_CODE)).then(Vec::new);
        }
    };
    Some(
        described
            .stack_resources()
            .iter()
            .filter_map(|resource| {
                // CloudFormation keeps reporting a resource a stack has disposed of, for as
                // long as it reports the stack; such a row is not a resource to discover.
                if resource
                    .resource_status()
                    .map(::aws_sdk_cloudformation::types::ResourceStatus::as_str)
                    == Some(DELETE_COMPLETE)
                {
                    return None;
                }
                // A resource the stack declared but never materialised carries no physical
                // identifier, so there is nothing to discover it by.
                let identifier = resource.physical_resource_id()?;
                if identifier.is_empty() {
                    return None;
                }
                Some((
                    identifier.to_string(),
                    resource.resource_type().map(ToString::to_string),
                    region.to_string(),
                ))
            })
            .collect(),
    )
}

/// The CloudFormation client a stack's resources are read through, against CloudFormation's
/// endpoint in `region`.
///
/// NOTHING here opens a socket or leaves the host: the transport is the callback, the
/// framework's seam into this same emulator, so the read is served by the router that
/// serves a client's own call and the host in `endpoint_url` is only how the SDK addresses
/// it. The credentials are the emulator's own, so the caller's `Authorization` header is
/// never forwarded. The transport is in-process, so retries, timeouts, stalled-stream
/// protection and identity caching — each of which needs a sleep implementation this
/// client is not given — are disabled.
fn cloudformation_client(
    callback: &EmbeddedEmulator,
    region: &str,
) -> ::aws_sdk_cloudformation::Client {
    let config = ::aws_sdk_cloudformation::Config::builder()
        .behavior_version(::aws_sdk_cloudformation::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_cloudformation::config::Region::new(
            region.to_string(),
        ))
        .endpoint_url(format!("https://{CLOUDFORMATION}.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_cloudformation::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_cloudformation::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_cloudformation::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_cloudformation::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_cloudformation::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A discovered resource with just the identifier filled in; the other members play no
    /// part in whether a resource was discovered.
    fn discovered(identifier: &str) -> DiscoveredResource {
        DiscoveredResource {
            identifier: identifier.to_string(),
            resource_type: None,
            input_source_id: "source".to_string(),
            input_source_type: INPUT_SOURCE_TYPE_CFN_STACK,
            aws_region: "us-east-1".to_string(),
        }
    }

    /// RULE: an identifier is reported undiscovered only where EVERY input source of the
    /// service was answered for; one that could not be read leaves nothing refused.
    #[test]
    fn nothing_is_undiscovered_while_a_source_is_unanswered() {
        let requested = vec!["held".to_string(), "absent".to_string()];
        for (case, every_source_answered, expected) in [
            ("every source answered", true, Some("absent")),
            ("a source nobody answered", false, None),
        ] {
            let discovery = Discovery {
                resources: vec![discovered("held")],
                every_source_answered,
            };
            let actual = discovery.first_undiscovered(&requested);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the resources a service discovered are exactly the identifiers its answered
    /// sources reported, whatever form those identifiers take.
    #[test]
    fn only_the_reported_identifiers_are_held() {
        let discovery = Discovery {
            resources: vec![
                discovered("arn:aws:sqs:us-east-1:000000000000:queue"),
                discovered("a-bucket-name"),
            ],
            every_source_answered: true,
        };
        for (case, identifier, expected) in [
            (
                "an ARN the source reported",
                "arn:aws:sqs:us-east-1:000000000000:queue",
                true,
            ),
            ("a bare name the source reported", "a-bucket-name", true),
            ("a prefix of a reported identifier", "a-bucket", false),
            ("an identifier no source reported", "another-bucket", false),
        ] {
            let actual = discovery.holds(identifier);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
