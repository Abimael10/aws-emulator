// SPDX-License-Identifier: CC0-1.0

//! Where a connector's bring-up ENDS: the resting `ConnectorState` the clock owes
//! a connector `CreateConnector` accepted, and the `stateDescription` that goes
//! with it.
//!
//! `CreateConnector` answers `CREATING` and the workers are brought up behind it,
//! so the create does not decide whether the connector WORKS — it decides what
//! the bring-up will find, and the clock reports it ([`ConnectorRecord`]'s
//! `resting_state`). A connector's workers exist to move records to and from the
//! Apache Kafka cluster its `kafkaCluster` names, and they are given nothing but
//! that cluster's bootstrap addresses to find it by: a bring-up that cannot reach
//! a cluster at any of those addresses has no cluster to run against, so the
//! connector comes to rest `FAILED` and reports WHY in its `stateDescription`,
//! which is the only place a client outside the connector's VPC can read it.
//!
//! WHICH clusters this account holds is not this service's fact: Amazon MSK owns
//! them, publishes the addresses a client bootstraps against
//! (`GetBootstrapBrokers`) and is asked here through the in-process callback, the
//! same seam `CreateCustomPlugin` resolves a plugin's file over
//! ([`super::take_on_custom_plugin_file`]). A refusal has to be grounded in what
//! the owning service answered, so an assembly with no Amazon MSK to ask
//! establishes NOTHING about the cluster and no failure is reported — the
//! position every cross-service pre-condition in this tree takes when the service
//! that owns the resource cannot be reached.

use super::*;

/// `ConnectorState` a connector whose bring-up did not reach a cluster comes to
/// rest in; one of [`CONNECTOR_STATE_VALUES`].
///
/// A RESTING state, not a transient one: the clock owes a connector that has
/// reached it nothing further, so it stays there until the connector is deleted —
/// and every call only a running connector serves is refused for it.
pub const CONNECTOR_FAILED: &str = "FAILED";

/// `StateDescription$code` of a connector whose workers found no Apache Kafka
/// cluster at the bootstrap addresses it was created with.
///
/// The model documents the member ("A code that describes the state of a
/// resource") and states no value set, so this names the one condition this
/// service brings a connector to rest `FAILED` for, and the message beside it
/// says which addresses were tried.
pub const UNREACHABLE_KAFKA_CLUSTER_CODE: &str = "UNABLE_TO_CONNECT_TO_KAFKA_CLUSTER";

/// How many clusters one bring-up asks Amazon MSK about: the largest page
/// `ListClustersV2` serves (`MaxResults` `range 1..100`).
///
/// ONE page, which is the bound on the work one create does: a Region holds
/// fewer clusters than that (Amazon MSK meters them per Region, and this
/// emulator's own cap is below this page size), so one page is every cluster an
/// endpoint's account can hold and there is nothing further to read.
const CLUSTER_PAGE_SIZE: i32 = 100;

/// What a connector's bring-up will find: the state the clock owes it once the
/// bring-up is done, and what to report about that state.
#[derive(Debug, Clone)]
pub struct BringUp {
    /// The `ConnectorState` the connector comes to rest in.
    pub resting_state: String,
    /// What `DescribeConnector` reports in `stateDescription` for that state, or
    /// `None` for a connector whose bring-up has nothing to explain.
    pub state_description: Option<StateDescription>,
}

impl BringUp {
    /// A bring-up that reaches its cluster: the connector's workers run, so it
    /// comes to rest `RUNNING` and there is nothing to describe about that state.
    pub fn running() -> Self {
        Self {
            resting_state: CONNECTOR_RUNNING.to_string(),
            state_description: None,
        }
    }

    /// A bring-up that found no Apache Kafka cluster at `bootstrap_servers`: the
    /// connector comes to rest `FAILED`, and the description names the addresses
    /// its workers were given, which is what a client needs to correct.
    pub fn no_cluster_at(bootstrap_servers: &str) -> Self {
        Self {
            resting_state: CONNECTOR_FAILED.to_string(),
            state_description: Some(StateDescription {
                code: Some(UNREACHABLE_KAFKA_CLUSTER_CODE.to_string()),
                message: Some(format!(
                    "The connector's workers could not connect to an Apache Kafka cluster at the \
                     bootstrap servers {bootstrap_servers}. No Apache Kafka cluster of this \
                     account is reachable at that address."
                )),
            }),
        }
    }
}

/// What the bring-up of a connector created against `bootstrap_servers` in
/// `region` will find — the rule in this module's documentation.
///
/// Called by `CreateConnector` with NO state guard held: it awaits, and it reads
/// nothing of this service's own store. The answer is stored on the connector,
/// so the clock reports the same outcome however often it is read, and the
/// cluster is asked about ONCE per connector rather than on every read.
pub async fn bring_up_against(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    bootstrap_servers: &str,
) -> BringUp {
    let addresses = bootstrap_addresses(bootstrap_servers);
    if addresses.is_empty() {
        // The member names no address at all, so there is nothing to ask Amazon
        // MSK about and nothing is established about any cluster.
        return BringUp::running();
    }
    match published_cluster_addresses(ctx, region).await {
        // Amazon MSK answered which clusters this account holds and where each
        // one is bootstrapped: a connector whose own addresses are none of them
        // has no cluster to run against.
        Some(published) => {
            if addresses.iter().any(|address| published.contains(address)) {
                BringUp::running()
            } else {
                BringUp::no_cluster_at(bootstrap_servers)
            }
        }
        // Not Amazon MSK reporting what it holds, so this establishes nothing
        // about the cluster: nothing is failed on an assumption.
        None => BringUp::running(),
    }
}

/// Every address a `bootstrapServers` value names, each lower-cased so two
/// spellings of one address are one address.
///
/// "The bootstrap servers of the cluster" (`ApacheKafkaCluster$bootstrapServers`)
/// is the value a Kafka client is configured with, which is Apache Kafka's own
/// `bootstrap.servers`: a comma-separated list of `host:port` pairs. Empty
/// entries and surrounding space are dropped, so a list a client wrote with
/// spaces after its commas names the same addresses as one without.
fn bootstrap_addresses(bootstrap_servers: &str) -> Vec<String> {
    bootstrap_servers
        .split(',')
        .map(|address| address.trim().to_ascii_lowercase())
        .filter(|address| !address.is_empty())
        .collect()
}

/// Every address Amazon MSK publishes for a cluster this account holds in
/// `region`, or `None` when Amazon MSK could not be asked at all.
///
/// `GetBootstrapBrokers` is the operation that answers "the connection string to
/// use to connect to the Apache Kafka cluster", one per access method the cluster
/// offers, so the union of those strings is every address a client of this
/// account's clusters bootstraps against. The clusters themselves come from one
/// `ListClustersV2` page ([`CLUSTER_PAGE_SIZE`]), which bounds the work one
/// create does.
///
/// `None` — establish nothing — for every answer that is NOT Amazon MSK reporting
/// what it holds: an emulator assembled with no Amazon MSK to ask, and an answer
/// that could not be read.
async fn published_cluster_addresses(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<Vec<String>> {
    let client = msk_client(ctx, region);
    let answered = match client
        .list_clusters_v2()
        .max_results(CLUSTER_PAGE_SIZE)
        .send()
        .await
    {
        Ok(answered) => answered,
        Err(error) => {
            // Worth a warning: an assembly with no Amazon MSK mounted to ask is
            // one where a connector's cluster cannot be resolved at all.
            ::tracing::warn!(
                region = %region,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the connector's Apache Kafka cluster could not be resolved against the \
                 emulator's Amazon MSK"
            );
            return None;
        }
    };
    let mut published = Vec::new();
    for cluster in answered.cluster_info_list() {
        let Some(cluster_arn) = cluster.cluster_arn() else {
            continue;
        };
        published.extend(bootstrap_addresses_of(&client, cluster_arn).await);
    }
    Some(published)
}

/// Every address the cluster `cluster_arn` names is bootstrapped at, as Amazon MSK
/// publishes them.
///
/// One `GetBootstrapBrokers` answer carries one connection string per access
/// method the cluster offers (plaintext, TLS, each SASL mechanism, the public and
/// the VPC-connectivity forms of each, and the IPv6 forms), and each string is
/// itself a comma-separated broker list, so they are read through the same
/// [`bootstrap_addresses`] reading a connector's own member is read through. A
/// cluster whose brokers Amazon MSK will not publish yet — one that is still
/// being created — publishes no address, exactly as a client that tried to
/// bootstrap against it would find.
async fn bootstrap_addresses_of(
    client: &::aws_sdk_kafka::Client,
    cluster_arn: &str,
) -> Vec<String> {
    let published = match client
        .get_bootstrap_brokers()
        .cluster_arn(cluster_arn)
        .send()
        .await
    {
        Ok(published) => published,
        Err(error) => {
            ::tracing::warn!(
                cluster_arn = %cluster_arn,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "Amazon MSK published no bootstrap address for a cluster it holds"
            );
            return Vec::new();
        }
    };
    [
        published.bootstrap_broker_string(),
        published.bootstrap_broker_string_tls(),
        published.bootstrap_broker_string_sasl_scram(),
        published.bootstrap_broker_string_sasl_iam(),
        published.bootstrap_broker_string_public_tls(),
        published.bootstrap_broker_string_public_sasl_scram(),
        published.bootstrap_broker_string_public_sasl_iam(),
        published.bootstrap_broker_string_vpc_connectivity_tls(),
        published.bootstrap_broker_string_vpc_connectivity_sasl_scram(),
        published.bootstrap_broker_string_vpc_connectivity_sasl_iam(),
        published.bootstrap_broker_string_ipv6(),
        published.bootstrap_broker_string_tls_ipv6(),
        published.bootstrap_broker_string_sasl_scram_ipv6(),
        published.bootstrap_broker_string_sasl_iam_ipv6(),
    ]
    .into_iter()
    .flatten()
    .flat_map(bootstrap_addresses)
    .collect()
}

/// An Amazon MSK client over the IN-PROCESS callback into this same emulator.
///
/// Nothing here opens a socket or leaves the host, exactly as for
/// [`super::take_on_custom_plugin_file`]'s Amazon S3 client: `ctx.callback()` is
/// the framework's documented seam for calling another emulated service, and the
/// `endpoint_url` below is an ADDRESS the in-process router reads rather than a
/// host any resolver looks up. `region` is the request's own signing region,
/// already narrowed to a Region label by [`request_region`] and re-established
/// here, so the caller cannot steer that authority.
///
/// The credentials are the emulator's own root credentials, which only sign this
/// operation's in-process calls; they are never put in a response or a log.
fn msk_client(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_kafka::Client {
    let region = if is_region_label(region) {
        region
    } else {
        DEFAULT_REGION
    };
    let config = ::aws_sdk_kafka::Config::builder()
        .behavior_version(::aws_sdk_kafka::config::BehaviorVersion::latest())
        .region(::aws_sdk_kafka::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://kafka.{region}.amazonaws.com"))
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .http_client(ctx.callback().http_client())
        // The transport is in-process: there is nothing to retry, time out or
        // watch for a stalled stream, and each of those would need a sleep
        // implementation this client is not given. The same goes for caching the
        // identity: the credentials are handed over directly.
        .retry_config(::aws_sdk_kafka::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_kafka::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_kafka::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_kafka::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_kafka::Client::from_conf(config)
}
