// SPDX-License-Identifier: CC0-1.0

//! The CloudFront half of the SDK seam a RUNNING handler calls another service through: the
//! DISTRIBUTIONS it reads and the INVALIDATION of a distribution's cache, built the way
//! CloudFront's own protocol binds them.
//!
//! A function that writes an object and then refreshes the distribution in front of it is
//! one of the shapes AWS documents a handler for: the write lands in the origin, the handler
//! FINDS the distribution serving it — `ListDistributions` over the account's distributions,
//! `GetDistribution` on one it already names — and then calls `CreateInvalidation` so the
//! edge stops serving what it holds. Each of those calls is only reachable from a handler if
//! this seam binds it, and a handler that cannot look a distribution up cannot name one to
//! invalidate.
//!
//! The other services a handler reaches bind their requests with an AWS JSON protocol,
//! where the request body IS the operation's input document, so one code path serves every
//! operation of every one of them ([`super::aws_api`]). CloudFront does not: its model is
//! `restXml`, so each operation binds its input to a METHOD, a PATH and an XML DOCUMENT of
//! its own, and answers XML. That binding is not written out here: the request is built,
//! signed, dispatched and parsed by the CloudFront CLIENT this crate holds
//! (`aws_sdk_cloudfront`), over the same in-process callback transport every other call of
//! this seam uses, so the wire details are the client's and the answer is the emulated
//! CloudFront service's own.
//!
//! What a guest can and cannot do here:
//!
//! * The destination is ALWAYS this emulator: the client is given the callback's own
//!   transport and CloudFront's own endpoint, so no host name is resolved and no socket is
//!   opened.
//! * The call is signed as the identity the invocation was given — the session of the
//!   function's execution role ([`super::execution_role_session`]) — so CloudFront judges it
//!   against the policies in force on that role exactly as it judges any other signed
//!   request.
//! * Only the operations in [`BOUND_OPERATIONS`] are bound, and only the input members each
//!   entry names. An operation or a member this seam does not bind is reported as a call that
//!   was NOT MADE, naming it, rather than sent with a value dropped — a dropped member would
//!   make a call that did something other than what the handler asked for.
//! * The answer's own status and document are what CloudFront answered: a success is that
//!   operation's output members, and a refusal is CloudFront's own error code, message and
//!   status, which the SDK the guest calls raises as `ClientError`
//!   ([`super::aws_sdk_guest`]).

use ::aws_sdk_cloudfront::Client;
use ::serde_json::{Map, Value};
use ::smithy_server_runtime::EmbeddedEmulator;

use super::aws_api::{Answer, CallerIdentity, Refusal};
use super::sdk_call::{
    insert_flag, insert_number, insert_text, insert_timestamp, Input, Observed, PROVIDER_NAME,
};

/// The service a refusal of one of these calls names, as the AWS SDKs name it.
const SERVICE: &str = "CloudFront";

/// The operations of CloudFront this seam binds, in the order a refusal lists them.
///
/// One entry per operation [`call`] builds a request for: the DISTRIBUTION READS a handler
/// resolves the distribution in front of an origin with, and the INVALIDATION surface it
/// refreshes that distribution's cache through. A handler naming any other operation of the
/// service is told the call was not made and why, because a request this module cannot bind
/// is one it would otherwise be guessing at.
pub const BOUND_OPERATIONS: &[&str] = &[
    "ListDistributions",
    "GetDistribution",
    "CreateInvalidation",
    "GetInvalidation",
    "ListInvalidations",
];

/// The Region a CloudFront call is signed for, whatever Region the client the guest built
/// names: CloudFront is a global service reached at one endpoint, and its credential scope
/// names the Region that endpoint is served from
/// (`service_common::management_events::PARTITION_LEADER_REGION`).
const SIGNING_REGION: &str = ::service_common::management_events::PARTITION_LEADER_REGION;

/// The endpoint every CloudFront call is made at, which is the host the emulated CloudFront
/// service claims its control plane on.
const ENDPOINT: &str = "https://cloudfront.amazonaws.com";

/// Make ONE CloudFront call: `operation`, with `body` as the operation's input document (the
/// keyword arguments the handler passed its client method).
pub async fn call(
    callback: &EmbeddedEmulator,
    identity: Option<&CallerIdentity>,
    operation: &str,
    body: &str,
    chain: Option<&str>,
) -> Result<Answer, Refusal> {
    let input = Input::read(SERVICE, body)?;
    let observed = Observed::of(SERVICE);
    let client = client(callback, identity, observed.clone(), chain);
    match operation {
        "ListDistributions" => list_distributions(&client, &input, &observed).await,
        "GetDistribution" => get_distribution(&client, &input, &observed).await,
        "CreateInvalidation" => create_invalidation(&client, &input, &observed).await,
        "GetInvalidation" => get_invalidation(&client, &input, &observed).await,
        "ListInvalidations" => list_invalidations(&client, &input, &observed).await,
        other => Err(Refusal::NotMade(format!(
            "the AWS SDK this runtime supplies binds no {other} call on CloudFront; the \
             operations it binds are {}",
            BOUND_OPERATIONS.join(", ")
        ))),
    }
}

/// A CloudFront client over the in-process callback into this same emulator, signing as the
/// identity this invocation was given.
///
/// The transport is in-process, so everything a network client needs and this one cannot
/// have — retries, timeouts, identity caching and stalled-stream protection, each of which
/// needs a sleep implementation the callback transport is not given — is switched off.
///
/// Every request it makes carries the tracing header of the chain this invocation belongs to,
/// where the invocation has one ([`::service_common::xray_trace::ChainHop`]): a call the
/// handler makes is one HOP of the chain that reached the handler.
fn client(
    callback: &EmbeddedEmulator,
    identity: Option<&CallerIdentity>,
    observed: Observed,
    chain: Option<&str>,
) -> Client {
    let config = ::aws_sdk_cloudfront::Config::builder()
        .behavior_version(::aws_sdk_cloudfront::config::BehaviorVersion::latest())
        .region(::aws_sdk_cloudfront::config::Region::new(
            SIGNING_REGION.to_string(),
        ))
        .endpoint_url(ENDPOINT)
        .http_client(callback.http_client())
        .interceptor(observed)
        .retry_config(::aws_sdk_cloudfront::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_cloudfront::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_cloudfront::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_cloudfront::config::StalledStreamProtectionConfig::disabled(),
        );
    let config = match identity {
        Some(identity) => config.credentials_provider(
            ::aws_sdk_cloudfront::config::Credentials::new(
                identity.access_key_id.clone(),
                identity.secret_access_key.clone(),
                identity.token().map(str::to_string),
                None,
                PROVIDER_NAME,
            ),
        ),
        None => config.credentials_provider(callback.aws_credentials_provider()),
    };
    // A call the handler makes is one HOP of the chain that reached the handler.
    let config = match chain {
        Some(chain) => config.interceptor(::service_common::xray_trace::ChainHop::carrying(chain)),
        None => config,
    };
    Client::from_conf(config.build())
}

/// `ListDistributions`: one page of the distributions the account holds, which is how a
/// handler finds the distribution serving the origin it just wrote to.
///
/// The page is reported as CloudFront answered it — the `Marker` the request carried, the
/// `MaxItems` it asked for, whether distributions remain and the id to resume at — with one
/// [`distribution_summary_document`] per distribution on it.
async fn list_distributions(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["Marker", "MaxItems"])?;
    let sent = client
        .list_distributions()
        .set_marker(input.text("Marker")?)
        .set_max_items(input.integer("MaxItems")?)
        .send()
        .await;
    let listed = match sent {
        Ok(listed) => listed,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    if let Some(list) = listed.distribution_list() {
        let mut page = Map::new();
        insert_text(&mut page, "Marker", Some(list.marker()));
        insert_text(&mut page, "NextMarker", list.next_marker());
        insert_number(&mut page, "MaxItems", Some(i64::from(list.max_items())));
        insert_number(&mut page, "Quantity", Some(i64::from(list.quantity())));
        page.insert("IsTruncated".to_string(), Value::Bool(list.is_truncated()));
        let items: Vec<Value> = list
            .items()
            .iter()
            .map(distribution_summary_document)
            .collect();
        if !items.is_empty() {
            page.insert("Items".to_string(), Value::Array(items));
        }
        document.insert("DistributionList".to_string(), Value::Object(page));
    }
    observed.answer(document)
}

/// `GetDistribution`: what ONE distribution reports now, for a handler that already names it.
///
/// `ETag` is the answer's own header member — "the current version of the distribution's
/// information" — so it is reported beside the distribution rather than inside it, which is
/// where the operation's output binds it.
async fn get_distribution(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["Id"])?;
    let sent = client
        .get_distribution()
        .id(input.required_text("Id")?)
        .send()
        .await;
    let read = match sent {
        Ok(read) => read,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "ETag", read.e_tag());
    if let Some(distribution) = read.distribution() {
        document.insert(
            "Distribution".to_string(),
            distribution_document(distribution),
        );
    }
    observed.answer(document)
}

/// One `DistributionSummary` as a listing's answer carries it: the identity a caller
/// addresses the distribution by (`Id`, `ARN`, `ETag`), the state it is in (`Status`,
/// `LastModifiedTime`, `Enabled`, `Staging`), the names it is served on (`DomainName`,
/// `Aliases`), where it serves from (`Origins`, `DefaultCacheBehavior`) and the remaining
/// scalar members of the delivery it describes.
///
/// The policy and certificate documents a distribution can also carry — its additional cache
/// behaviours, custom error responses, origin groups, viewer certificate and geographic
/// restrictions — are not read out here: what a handler does with a distribution through this
/// seam is FIND the one in front of an origin and invalidate it, and no operation this seam
/// binds writes a configuration back, so nothing a handler can call needs them.
fn distribution_summary_document(
    summary: &::aws_sdk_cloudfront::types::DistributionSummary,
) -> Value {
    let mut document = Map::new();
    insert_text(&mut document, "Id", Some(summary.id()));
    insert_text(&mut document, "ARN", Some(summary.arn()));
    insert_text(&mut document, "ETag", summary.e_tag());
    insert_text(&mut document, "Status", Some(summary.status()));
    insert_timestamp(
        &mut document,
        "LastModifiedTime",
        Some(summary.last_modified_time()),
    );
    insert_text(&mut document, "DomainName", Some(summary.domain_name()));
    if let Some(aliases) = summary.aliases() {
        document.insert("Aliases".to_string(), aliases_document(aliases));
    }
    if let Some(origins) = summary.origins() {
        document.insert("Origins".to_string(), origins_document(origins));
    }
    if let Some(behavior) = summary.default_cache_behavior() {
        document.insert(
            "DefaultCacheBehavior".to_string(),
            default_cache_behavior_document(behavior),
        );
    }
    insert_text(&mut document, "Comment", Some(summary.comment()));
    insert_text(
        &mut document,
        "PriceClass",
        Some(summary.price_class().as_str()),
    );
    insert_flag(&mut document, "Enabled", Some(summary.enabled()));
    insert_text(
        &mut document,
        "HttpVersion",
        Some(summary.http_version().as_str()),
    );
    insert_flag(
        &mut document,
        "IsIPV6Enabled",
        Some(summary.is_ipv6_enabled()),
    );
    insert_text(&mut document, "WebACLId", Some(summary.web_acl_id()));
    insert_flag(&mut document, "Staging", Some(summary.staging()));
    Value::Object(document)
}

/// One `Distribution` as a read's answer carries it: the same identity and state members a
/// summary reports, the number of invalidation batches still in progress, and the
/// CONFIGURATION the distribution is served under ([`distribution_config_document`]).
fn distribution_document(distribution: &::aws_sdk_cloudfront::types::Distribution) -> Value {
    let mut document = Map::new();
    insert_text(&mut document, "Id", Some(distribution.id()));
    insert_text(&mut document, "ARN", Some(distribution.arn()));
    insert_text(&mut document, "Status", Some(distribution.status()));
    insert_timestamp(
        &mut document,
        "LastModifiedTime",
        Some(distribution.last_modified_time()),
    );
    insert_number(
        &mut document,
        "InProgressInvalidationBatches",
        Some(i64::from(distribution.in_progress_invalidation_batches())),
    );
    insert_text(
        &mut document,
        "DomainName",
        Some(distribution.domain_name()),
    );
    if let Some(config) = distribution.distribution_config() {
        document.insert(
            "DistributionConfig".to_string(),
            distribution_config_document(config),
        );
    }
    Value::Object(document)
}

/// One `DistributionConfig` as a read's answer carries it: the reference the create was made
/// under, what the distribution serves on and from, and the scalar members of the delivery it
/// configures — the same projection a summary carries, for the same reason.
fn distribution_config_document(
    config: &::aws_sdk_cloudfront::types::DistributionConfig,
) -> Value {
    let mut document = Map::new();
    insert_text(
        &mut document,
        "CallerReference",
        Some(config.caller_reference()),
    );
    if let Some(aliases) = config.aliases() {
        document.insert("Aliases".to_string(), aliases_document(aliases));
    }
    insert_text(
        &mut document,
        "DefaultRootObject",
        config.default_root_object(),
    );
    if let Some(origins) = config.origins() {
        document.insert("Origins".to_string(), origins_document(origins));
    }
    if let Some(behavior) = config.default_cache_behavior() {
        document.insert(
            "DefaultCacheBehavior".to_string(),
            default_cache_behavior_document(behavior),
        );
    }
    insert_text(&mut document, "Comment", Some(config.comment()));
    insert_text(
        &mut document,
        "PriceClass",
        config.price_class().map(|class| class.as_str()),
    );
    insert_flag(&mut document, "Enabled", Some(config.enabled()));
    insert_text(
        &mut document,
        "HttpVersion",
        config.http_version().map(|version| version.as_str()),
    );
    insert_flag(&mut document, "IsIPV6Enabled", config.is_ipv6_enabled());
    insert_text(&mut document, "WebACLId", config.web_acl_id());
    insert_flag(&mut document, "Staging", config.staging());
    Value::Object(document)
}

/// The `Aliases` of a distribution: the alternate domain names it answers on, with the
/// `Quantity` CloudFront reports beside them.
fn aliases_document(aliases: &::aws_sdk_cloudfront::types::Aliases) -> Value {
    let mut document = Map::new();
    insert_number(
        &mut document,
        "Quantity",
        Some(i64::from(aliases.quantity())),
    );
    let items: Vec<Value> = aliases
        .items()
        .iter()
        .map(|alias| Value::String(alias.clone()))
        .collect();
    if !items.is_empty() {
        document.insert("Items".to_string(), Value::Array(items));
    }
    Value::Object(document)
}

/// The `Origins` of a distribution: where the content it serves is fetched from, which is
/// what a handler matches a distribution against the object store it just wrote to by.
fn origins_document(origins: &::aws_sdk_cloudfront::types::Origins) -> Value {
    let mut document = Map::new();
    insert_number(
        &mut document,
        "Quantity",
        Some(i64::from(origins.quantity())),
    );
    let items: Vec<Value> = origins
        .items()
        .iter()
        .map(|origin| {
            let mut entry = Map::new();
            insert_text(&mut entry, "Id", Some(origin.id()));
            insert_text(&mut entry, "DomainName", Some(origin.domain_name()));
            insert_text(&mut entry, "OriginPath", origin.origin_path());
            insert_text(
                &mut entry,
                "OriginAccessControlId",
                origin.origin_access_control_id(),
            );
            Value::Object(entry)
        })
        .collect();
    if !items.is_empty() {
        document.insert("Items".to_string(), Value::Array(items));
    }
    Value::Object(document)
}

/// The `DefaultCacheBehavior` of a distribution: which origin an unmatched request is routed
/// to and the protocol a viewer reaches it over.
fn default_cache_behavior_document(
    behavior: &::aws_sdk_cloudfront::types::DefaultCacheBehavior,
) -> Value {
    let mut document = Map::new();
    insert_text(
        &mut document,
        "TargetOriginId",
        Some(behavior.target_origin_id()),
    );
    insert_text(
        &mut document,
        "ViewerProtocolPolicy",
        Some(behavior.viewer_protocol_policy().as_str()),
    );
    Value::Object(document)
}

/// `CreateInvalidation`: the batch of paths whose objects the edge is to stop serving.
async fn create_invalidation(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["DistributionId", "InvalidationBatch"])?;
    let batch = input.structure("InvalidationBatch")?.ok_or_else(|| {
        Refusal::NotMade(
            "the CloudFront call carries no InvalidationBatch, which names the paths to \
             invalidate"
                .to_string(),
        )
    })?;
    let paths = batch.structure("Paths")?.ok_or_else(|| {
        Refusal::NotMade(
            "the call's InvalidationBatch carries no Paths, which names the objects to \
             invalidate"
                .to_string(),
        )
    })?;
    // A member of a NESTED structure this binding does not carry stops the call the same way
    // a top-level one does: the whole request has to be the one the handler asked for, at
    // every depth it states.
    batch.only(&["Paths", "CallerReference"])?;
    paths.only(&["Items", "Quantity"])?;
    let items = paths.text_list("Items")?;
    // `Paths$Quantity` is "the number of invalidation paths specified for the objects that
    // you want to invalidate", so it is the number of items the call carries — the value an
    // SDK computes for the caller rather than one the handler states, which is why a call
    // that states it is sent with the count of the items it carries.
    let quantity = i32::try_from(items.len()).unwrap_or(i32::MAX);
    let batch_sent = ::aws_sdk_cloudfront::types::InvalidationBatch::builder()
        .paths(
            ::aws_sdk_cloudfront::types::Paths::builder()
                .quantity(quantity)
                .set_items(Some(items))
                .build()
                .map_err(|error| {
                    Refusal::NotMade(format!(
                        "the call's Paths is not one CloudFront accepts: {error}"
                    ))
                })?,
        )
        .set_caller_reference(batch.text("CallerReference")?)
        .build()
        .map_err(|error| {
            Refusal::NotMade(format!(
                "the call's InvalidationBatch is not one CloudFront accepts: {error}"
            ))
        })?;
    let sent = client
        .create_invalidation()
        .distribution_id(input.required_text("DistributionId")?)
        .invalidation_batch(batch_sent)
        .send()
        .await;
    let created = match sent {
        Ok(created) => created,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "Location", created.location());
    if let Some(invalidation) = created.invalidation() {
        document.insert("Invalidation".to_string(), invalidation_document(invalidation));
    }
    observed.answer(document)
}

/// `GetInvalidation`: what one submitted invalidation reports now.
async fn get_invalidation(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["DistributionId", "Id"])?;
    let sent = client
        .get_invalidation()
        .distribution_id(input.required_text("DistributionId")?)
        .id(input.required_text("Id")?)
        .send()
        .await;
    let read = match sent {
        Ok(read) => read,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    if let Some(invalidation) = read.invalidation() {
        document.insert("Invalidation".to_string(), invalidation_document(invalidation));
    }
    observed.answer(document)
}

/// `ListInvalidations`: one page of the invalidations submitted for a distribution.
async fn list_invalidations(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["DistributionId", "Marker", "MaxItems"])?;
    let sent = client
        .list_invalidations()
        .distribution_id(input.required_text("DistributionId")?)
        .set_marker(input.text("Marker")?)
        .set_max_items(input.integer("MaxItems")?)
        .send()
        .await;
    let listed = match sent {
        Ok(listed) => listed,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    if let Some(list) = listed.invalidation_list() {
        let mut page = Map::new();
        insert_text(&mut page, "Marker", Some(list.marker()));
        insert_text(&mut page, "NextMarker", list.next_marker());
        insert_number(&mut page, "MaxItems", Some(i64::from(list.max_items())));
        insert_number(&mut page, "Quantity", Some(i64::from(list.quantity())));
        page.insert(
            "IsTruncated".to_string(),
            Value::Bool(list.is_truncated()),
        );
        let items: Vec<Value> = list
            .items()
            .iter()
            .map(|summary| {
                let mut entry = Map::new();
                entry.insert("Id".to_string(), Value::String(summary.id().to_string()));
                entry.insert(
                    "Status".to_string(),
                    Value::String(summary.status().to_string()),
                );
                insert_timestamp(&mut entry, "CreateTime", Some(summary.create_time()));
                Value::Object(entry)
            })
            .collect();
        if !items.is_empty() {
            page.insert("Items".to_string(), Value::Array(items));
        }
        document.insert("InvalidationList".to_string(), Value::Object(page));
    }
    observed.answer(document)
}

/// One `Invalidation` as the answer's document carries it: what the service reported about
/// the batch it holds.
fn invalidation_document(invalidation: &::aws_sdk_cloudfront::types::Invalidation) -> Value {
    let mut document = Map::new();
    document.insert(
        "Id".to_string(),
        Value::String(invalidation.id().to_string()),
    );
    document.insert(
        "Status".to_string(),
        Value::String(invalidation.status().to_string()),
    );
    insert_timestamp(&mut document, "CreateTime", Some(invalidation.create_time()));
    if let Some(batch) = invalidation.invalidation_batch() {
        let mut batch_document = Map::new();
        batch_document.insert(
            "CallerReference".to_string(),
            Value::String(batch.caller_reference().to_string()),
        );
        if let Some(paths) = batch.paths() {
            let mut paths_document = Map::new();
            paths_document.insert(
                "Quantity".to_string(),
                Value::Number(i64::from(paths.quantity()).into()),
            );
            let items: Vec<Value> = paths
                .items()
                .iter()
                .map(|path| Value::String(path.clone()))
                .collect();
            if !items.is_empty() {
                paths_document.insert("Items".to_string(), Value::Array(items));
            }
            batch_document.insert("Paths".to_string(), Value::Object(paths_document));
        }
        document.insert(
            "InvalidationBatch".to_string(),
            Value::Object(batch_document),
        );
    }
    Value::Object(document)
}

#[cfg(test)]
mod tests {
    use super::{Input, BOUND_OPERATIONS, SERVICE};

    #[test]
    fn every_bound_operation_is_named_once() {
        let mut names = BOUND_OPERATIONS.to_vec();
        names.sort_unstable();
        let total = names.len();
        names.dedup();
        assert_eq!(
            names.len(),
            total,
            "case \"the bound operations\": expected each operation to be named once, got \
             {BOUND_OPERATIONS:?}"
        );
    }

    #[test]
    fn an_input_is_read_as_the_operations_own_members() {
        // Rows differ along the shape of the value a member carries: text, a number, a
        // nested structure, a list of text, and a value of the wrong shape for each.
        let input = Input::read(
            SERVICE,
            r#"{"DistributionId":"E1EXAMPLE","MaxItems":5,
                "InvalidationBatch":{"CallerReference":"ref-1",
                "Paths":{"Items":["/a","/b"]}},"Wrong":{"a":1}}"#,
        )
        .expect("the document is an object");
        assert_eq!(
            input.required_text("DistributionId").ok(),
            Some("E1EXAMPLE".to_string()),
            "case \"a text member\": expected the distribution id"
        );
        assert_eq!(
            input.integer("MaxItems").ok().flatten(),
            Some(5),
            "case \"a numeric member\": expected 5"
        );
        let batch = input
            .structure("InvalidationBatch")
            .ok()
            .flatten()
            .expect("case \"a nested structure\": expected the batch");
        assert_eq!(
            batch.text("CallerReference").ok().flatten(),
            Some("ref-1".to_string()),
            "case \"a nested text member\": expected the caller reference"
        );
        let paths = batch
            .structure("Paths")
            .ok()
            .flatten()
            .expect("case \"a doubly nested structure\": expected the paths");
        assert_eq!(
            paths.text_list("Items").ok(),
            Some(vec!["/a".to_string(), "/b".to_string()]),
            "case \"a list of text\": expected both paths"
        );
        assert!(
            paths.text_list("Absent").is_ok_and(|items| items.is_empty()),
            "case \"an absent list\": expected no entries"
        );
        assert!(
            input.text("MaxItems").is_err(),
            "case \"a number read as text\": expected a call that was not made"
        );
        assert!(
            input.integer("DistributionId").is_err(),
            "case \"text read as a number\": expected a call that was not made"
        );
        assert!(
            input.structure("DistributionId").is_err(),
            "case \"text read as a structure\": expected a call that was not made"
        );
        assert!(
            input.only(&["DistributionId", "MaxItems", "InvalidationBatch"]).is_err(),
            "case \"an unbound member\": expected a call that was not made"
        );
        // A member of a NESTED structure this binding does not carry stops the call the same
        // way a top-level one does, which is what `create_invalidation` applies to the batch
        // and to its paths.
        assert!(
            batch.only(&["Paths"]).is_err(),
            "case \"an unbound nested member\": expected a call that was not made"
        );
    }
}
