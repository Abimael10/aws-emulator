// SPDX-License-Identifier: CC0-1.0

//! The subnets a scraper's SOURCE names: a fact this service resolves rather than holds.
//!
//! `CreateScraper$source` carries the subnets the scraper reaches its source through
//! (`EksConfiguration$subnetIds`, `VpcConfiguration$subnetIds` — the two variants of the
//! modelled `Source` union). A subnet is not a resource of this service: it is Amazon
//! EC2's, so what a request names is resolved against EC2, reached the way any emulated
//! service reaches another — through the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), so this crate names no other
//! service's crate and nothing leaves the process. `services/common/src/network.rs` states
//! that contract for every service that places a resource in a subnet.
//!
//! This service only GATES on existence: it stores the `source` the request carried and
//! reports no VPC, zone or interface of its own, so there is nothing of the subnet to keep.
//! That reading decides two things:
//!
//! * A hop that could NOT be made (no EC2 in this process, a Region label this endpoint
//!   cannot be addressed by, an answer that could not be read) is evidence of nothing, so
//!   it refuses nothing and says so at `warn`. A refusal is never assumed from silence.
//! * What the caller is told is what EC2 ANSWERED — the sentence EC2 states about the
//!   subnet, carried through verbatim inside the envelope the SDK layer that relayed the
//!   call states around it (`service_common::relayed_refusal`), which is how the service
//!   reports a refusal it got from its EC2 hop rather than wording one of its own.
//!
//! Nothing is cached: a subnet created a moment ago resolves at once, and one that was
//! deleted stops resolving at once.

use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

use crate::ops::types::Source;

/// The peer this module asks, as the in-process endpoint names it.
const EC2_PEER: &str = "ec2";

/// The called service's name as the SDK layer that relayed the call reports it in a
/// relayed refusal's envelope — neither the endpoint prefix above nor the casing of EC2's
/// `sdkId`.
const EC2_RELAY_SERVICE: &str = "Ec2";

/// The prefix of every EC2 error code that says a value names no subnet of the account:
/// `InvalidSubnetID.NotFound` for an id no subnet carries, `InvalidSubnetID.Malformed` for
/// a value that is not a subnet id at all. Both say the same thing about a source.
const SUBNET_ID_REFUSAL_PREFIX: &str = "InvalidSubnetID.";

/// The most subnet identifiers ONE create resolves, as the ids of one `DescribeSubnets`
/// call.
///
/// CHOSEN as a bound on the work one request does, not a published AWS quota (neither
/// `subnetIds` list is modelled with a `length`): the list comes from the request and is
/// cloned into the hop below, so without a ceiling one create would size that hop. It sits
/// far above the handful of Availability Zones a cluster is really spread over. Ids past
/// it are not resolved, so the ceiling can only leave a refusal unmade — it never invents
/// one.
const MAX_RESOLVED_SUBNET_IDS: usize = 64;

/// The subnets a scraper's `source` names, whichever variant of the union it is: both
/// `eksConfiguration` and `vpcConfiguration` carry a `subnetIds` list, and the subnets are
/// the account's either way.
pub fn source_subnet_ids(source: &Source) -> &[String] {
    match source {
        Source::eksConfiguration(configuration) => &configuration.subnet_ids,
        Source::vpcConfiguration(configuration) => &configuration.subnet_ids,
    }
}

/// An EC2 client for `region` over the in-process callback into this same emulator.
///
/// The endpoint URL is only the address that transport routes on IN PROCESS: no socket is
/// opened, no name is resolved and nothing leaves this process. The credentials are the
/// callback's, which sign this in-process hop and never appear in a response.
///
/// The Region a request is signed for is CALLER-SUPPLIED text and here it ends up in an
/// endpoint URL's authority, so it is read as a Region LABEL
/// (`service_common::arn::region_label`, the one place this tree states that grammar)
/// instead of being interpolated as given: `None` for a value that would name an authority
/// the caller chose rather than the peer service this emulator asks.
fn ec2(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<::aws_sdk_ec2::Client> {
    let Some(region) = ::service_common::arn::region_label(region) else {
        ::tracing::warn!("the request names no Region label, so EC2 cannot be asked");
        return None;
    };
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_ec2::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://{EC2_PEER}.{region}.amazonaws.com"))
        // The transport is in-process: there is nothing to retry, time out or watch for a
        // stalled stream, and each of those would need a sleep implementation this client
        // is not given. The same goes for caching the identity: the credentials the
        // callback hands over are used directly.
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    Some(::aws_sdk_ec2::Client::from_conf(config))
}

/// Why the subnets `subnet_ids` names cannot be scraped through, or `None` when nothing
/// says they cannot — which includes every case where EC2 could not be asked, because a
/// refusal has to be grounded in what EC2 answered rather than assumed from silence.
///
/// The ids are deduplicated, so the hop does not grow with how many times one subnet is
/// named, and bounded by [`MAX_RESOLVED_SUBNET_IDS`].
///
/// The message is EC2's own sentence about the subnet, carried through verbatim, followed
/// by the envelope naming the relayed call: the service it was made to, the status EC2
/// answered it with, that call's request id and the one attempt it took. The request id is
/// the relayed CALL's, minted per call by `service_common::relayed_refusal`, so it is not
/// derived from anything in the request.
pub async fn source_subnet_refusal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_ids: &[String],
) -> Option<String> {
    let mut asked: Vec<String> = Vec::new();
    for subnet_id in subnet_ids {
        if asked.len() >= MAX_RESOLVED_SUBNET_IDS {
            break;
        }
        if !asked.iter().any(|seen| seen == subnet_id) {
            asked.push(subnet_id.clone());
        }
    }
    if asked.is_empty() {
        return None;
    }

    let client = ec2(ctx, region)?;
    let error = match client
        .describe_subnets()
        .set_subnet_ids(Some(asked))
        .send()
        .await
    {
        // EC2 answers for an id it holds no subnet of rather than leaving the row out, so
        // an answer it gave is one it resolved every id of. An answer that somehow omits
        // one states no sentence about it, and a sentence of this service's own would be
        // the emulator wording EC2's refusal for it.
        Ok(_) => return None,
        Err(error) => error,
    };

    let service_error = error.as_service_error();
    let code = service_error
        .and_then(ProvideErrorMetadata::code)
        .unwrap_or_default();
    let reason = service_error
        .and_then(ProvideErrorMetadata::message)
        .unwrap_or_default();
    // What EC2 answered is a refusal of these ids only when it says so in its own error
    // code AND states a sentence to carry through. The status below is the one EC2 really
    // answered with, read off that answer, because an envelope pairing a service's name
    // with a status it did not give misattributes the failure.
    //
    // Anything else is this emulator failing to ASK rather than EC2 reporting a subnet the
    // account does not have, so it grounds no refusal — and it is worth a warning, because a
    // deployment with no EC2 to ask is one where this gate does not run.
    let answered = code.starts_with(SUBNET_ID_REFUSAL_PREFIX) && !reason.is_empty();
    let status = error
        .raw_response()
        .map(|response| response.status().as_u16());
    let (true, Some(status)) = (answered, status) else {
        ::tracing::warn!(
            %region,
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "the subnets a scraper's source named could not be resolved against EC2"
        );
        return None;
    };

    Some(::service_common::relayed_refusal::relayed_refusal_message(
        reason,
        ::service_common::relayed_refusal::RelayedCall::once(EC2_RELAY_SERVICE, status),
    ))
}
