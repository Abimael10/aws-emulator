// SPDX-License-Identifier: CC0-1.0

//! The ENDPOINTS of an RDS proxy: the one every proxy publishes without anybody creating
//! it, the ADDITIONAL ones a caller creates, and the projection the proxy-endpoint report
//! answers with.
//!
//! "RDS automatically creates one endpoint for each DB proxy" (`DBProxyEndpoint`), and that
//! endpoint is the proxy's DEFAULT one — "default DB proxy endpoints always have read/write
//! capability" (`DBProxyEndpoint$IsDefault`). It is therefore no record of its own: every
//! member it reports is a function of the proxy it belongs to — the address that proxy
//! publishes, the network it listens on, the subnets and security groups it was placed in,
//! and the state it is in — so a proxy that was renamed, re-addressed or taken away cannot
//! leave an endpoint reporting what the proxy no longer is.
//!
//! "For Aurora DB clusters, you can associate additional endpoints with the same DB proxy.
//! These endpoints can be read/write or read-only. They can also reside in different VPCs
//! than the associated DB proxy" (same shape), and those are the endpoints
//! `CreateDBProxyEndpoint` mints: each carries a name a caller chose, an ARN, the role it
//! serves and the network it was placed in, so each is a RECORD of its own
//! ([`DbProxyCustomEndpointRecord`]). What it does NOT carry is anything about the proxy
//! behind it: the proxy is read beside the record when the endpoint is projected, for the
//! same reason the default endpoint is a pure function of the proxy.

use super::*;
use crate::ops::types::{DBProxyEndpoint, D_B_PROXY_ENDPOINT_STATUS_VALUES};

/// The ARN resource type of a proxy's endpoint, as AWS writes it under the `rds` namespace.
const ENDPOINT_ARN_RESOURCE_TYPE: &str = "db-proxy-endpoint";

/// `DBProxyEndpointStatus` — "a status of `available` means the endpoint is ready to handle
/// requests".
pub const DB_PROXY_ENDPOINT_STATUS_AVAILABLE: &str = "available";

/// `DBProxyEndpointTargetRole` — "`READ_WRITE`", the role of an endpoint that "can be used
/// for read/write" operations, which is what a proxy's default endpoint always is.
pub const DB_PROXY_ENDPOINT_TARGET_ROLE_READ_WRITE: &str = "READ_WRITE";

/// `DBProxyEndpoint$DBProxyEndpointArn` — "the Amazon Resource Name (ARN) for the DB proxy
/// endpoint", under the resource type AWS gives a proxy endpoint.
pub fn db_proxy_endpoint_arn(region: &str, endpoint_name: &str) -> String {
    emulator_server::emulator_arn(
        ARN_NAMESPACE,
        region,
        &format!("{ENDPOINT_ARN_RESOURCE_TYPE}:{endpoint_name}"),
    )
}

/// The message a `DBProxyEndpointNotFoundFault` carries for a `DBProxyEndpointName` that
/// resolves no endpoint: the fault reports "the DB proxy endpoint doesn't exist", and Amazon
/// RDS words it as a statement about the resource type the API spells `DBProxyEndpoint`,
/// with the requested name in SINGLE QUOTES — the wording and quoting every not-found
/// sentence of the proxy API shares ([`db_proxy_not_found_message`]).
///
/// It lives here rather than in one handler because every operation that resolves a
/// `DBProxyEndpointName` answers the same one fault. `named` is the value the REQUEST
/// carried, which is the spelling an endpoint is reported under too (`DBProxyEndpointName`
/// is not folded).
pub fn db_proxy_endpoint_not_found_message(named: &str) -> String {
    format!("DBProxyEndpoint '{named}' not found.")
}

/// What the endpoint of a proxy in `proxy_status` reports as its own `Status`.
///
/// An endpoint's status list — "available, modifying, incompatible-network,
/// insufficient-resource-limits, creating, deleting" (`DBProxyEndpointStatus`) — is NARROWER
/// than the proxy's, so the proxy's state is read onto it: a proxy still being provisioned
/// is bringing its endpoint up with it, a proxy applying a change is applying it to the
/// endpoint clients reach it through, and a proxy whose delete was accepted takes its
/// endpoint with it, so every status the two vocabularies share is reported as it stands. A
/// proxy status the endpoint's own list does NOT carry leaves the endpoint
/// [`DB_PROXY_ENDPOINT_STATUS_AVAILABLE`], because the endpoint is the address a client
/// connects to and the proxy behind it still answers.
pub fn proxy_endpoint_status(proxy_status: &str) -> &'static str {
    D_B_PROXY_ENDPOINT_STATUS_VALUES
        .iter()
        .find(|shared| **shared == proxy_status)
        .copied()
        .unwrap_or(DB_PROXY_ENDPOINT_STATUS_AVAILABLE)
}

/// `DBProxyEndpointStatus` — the status an endpoint a caller created reports while this
/// service brings it up, from that enum's published list.
pub const DB_PROXY_ENDPOINT_STATUS_CREATING: &str = "creating";

/// `DBProxyEndpointStatus` — the status an endpoint reports once a delete has been accepted
/// for it, or once the proxy it belongs to is on its way out and takes it along, from the
/// same published list.
pub const DB_PROXY_ENDPOINT_STATUS_DELETING: &str = "deleting";

/// `DBProxyEndpointTargetRole` — "`READ_ONLY`", the role of an endpoint that "can be used
/// for ... only read operations".
pub const DB_PROXY_ENDPOINT_TARGET_ROLE_READ_ONLY: &str = "READ_ONLY";

/// How many endpoints a caller may add to one proxy. The cap is this emulator's, and it
/// bounds the store the same way [`DB_PROXY_QUOTA_PER_REGION`] does: an entry exists only
/// for a proxy that quota already counts, so the two together bound the endpoints one region
/// holds. A create past it is the modelled `DBProxyEndpointQuotaExceededFault`.
pub const DB_PROXY_ENDPOINT_QUOTA_PER_PROXY: usize = 20;

/// The sentence a `DBProxyEndpointQuotaExceededFault` carries, as the model publishes it for
/// the error class: "The DB proxy already has the maximum number of endpoints." It names
/// neither the proxy nor the allowance, so it is a constant rather than a `format!` over the
/// request.
pub const DB_PROXY_ENDPOINT_QUOTA_MESSAGE: &str =
    "The DB proxy already has the maximum number of endpoints.";

/// The message a `DBProxyEndpointAlreadyExistsFault` carries for a name the account already
/// holds an endpoint under: the fault reports that "the specified DB proxy endpoint name
/// must be unique for all DB proxy endpoints owned by your Amazon Web Services account in
/// the specified Amazon Web Services Region".
///
/// Amazon RDS words it as a statement about the resource that exists, naming the resource
/// type as the API spells it (`DBProxyEndpoint`) with the requested name in SINGLE QUOTES
/// and no terminal period — the quoting and the form the proxy API's own already-exists
/// sentence uses ([`db_proxy_already_exists_message`]), which is part of the text a caller
/// matches the refusal against. `named` is the value the REQUEST carried, which is the
/// spelling an endpoint is stored under too (`DBProxyEndpointName` is not folded).
pub fn db_proxy_endpoint_already_exists_message(named: &str) -> String {
    format!("The DBProxyEndpoint '{named}' already exists")
}

/// An endpoint a caller ADDED to a DB proxy: everything this service persists about it.
///
/// The ARN, the published address and the status it reports are not stored as text: the ARN
/// is a function of the stored region and name, and the address and the reported status are
/// functions of the record and of the PROXY it belongs to ([`Self::to_wire`]) — so an
/// endpoint cannot be left reporting an address, or a state, of a proxy that has been
/// renamed, re-addressed or taken away.
#[derive(Debug)]
pub struct DbProxyCustomEndpointRecord {
    /// `DBProxyEndpointName`, which is half this record's key. Not folded: the member
    /// publishes no folding rule, so the endpoint is reported under the spelling the create
    /// carried.
    pub name: String,
    /// The region whose credential scope the create was signed with; the scope of the
    /// endpoint's ARN and of the proxy it belongs to.
    pub region: String,
    /// `DBProxyName`: "the identifier for the DB proxy that is associated with this DB proxy
    /// endpoint", resolved in the store before the endpoint was stored, and moved with the
    /// proxy when `ModifyDBProxy` renames it.
    pub db_proxy_name: String,
    /// `TargetRole`: "a value that indicates whether the DB proxy endpoint can be used for
    /// read/write or read-only operations" — what the create asked for, or the role
    /// `CreateDBProxyEndpoint$TargetRole` documents as its default ("The default is
    /// READ_WRITE").
    pub target_role: String,
    /// `VpcSubnetIds`: "the EC2 subnet IDs for the DB proxy endpoint", which may be "a
    /// different set of subnet IDs than for the original DB proxy".
    pub vpc_subnet_ids: Vec<String>,
    /// `VpcId`: the VPC those subnets belong to, as the service that owns subnets answered
    /// for them when the endpoint was created — "they can also reside in different VPCs than
    /// the associated DB proxy", so it is read for the endpoint's own subnets rather than
    /// copied off the proxy. `None` where that question could not be answered, which is
    /// reported as no VPC rather than one of this service's choosing.
    pub vpc_id: Option<String>,
    /// `VpcSecurityGroupIds`: the groups the endpoint belongs to — the ones the request
    /// named, or the default security group of the VPC it was placed in where it named none.
    pub vpc_security_group_ids: Vec<String>,
    /// `EndpointNetworkType`, or the default `CreateDBProxyEndpoint$EndpointNetworkType`
    /// documents ("Default: IPV4").
    pub endpoint_network_type: String,
    /// The minted label this endpoint's own LISTENER is resolved by, minted once and never
    /// changed, so a session open on the endpoint survives a rename of the proxy.
    ///
    /// A label of its OWN rather than the proxy's ([`DbProxyRecord::endpoint_host`]): a
    /// session that arrived here is served in the role THIS endpoint has
    /// (`super::data_plane::proxy_endpoint`), which a label shared with the proxy could not
    /// express.
    pub endpoint_label: String,
    /// The address of this endpoint's own listener, for a proxy whose engine family's wire
    /// this emulator speaks: what [`Self::to_wire`] publishes, because a client connects to
    /// exactly the string the control plane hands it.
    ///
    /// `None` for a proxy of a family whose wire this emulator does not speak, which gets no
    /// listener and publishes the DNS-form address RDS gives a proxy endpoint, exactly as the
    /// proxy's own endpoint does ([`DbProxyRecord::endpoint`]).
    pub served_endpoint: Option<ServedEndpoint>,
    /// `Status`: what the endpoint itself is doing, which [`Self::to_wire`] reports unless
    /// the proxy behind it is on its way out.
    pub status: String,
    /// The transition the clock owes this record, if any.
    pub pending_status: Option<PendingStatus>,
    /// `CreatedDate`: "the date and time when the DB proxy endpoint was first created".
    pub created_date: ::aws_smithy_types::DateTime,
    /// The tags the create assigned to the endpoint. The modelled `DBProxyEndpoint` shape
    /// carries no tag list, so these are held for the tagging operations rather than
    /// projected onto the endpoint.
    pub tags: Vec<Tag>,
}

impl DbProxyCustomEndpointRecord {
    /// Apply a change whose due instant has passed. Returns whether the record is now gone,
    /// so the caller drops it from the store — the same contract as
    /// [`DbClusterEndpointRecord::settle`].
    pub(super) fn settle(&mut self, now: Instant) -> bool {
        let due = self
            .pending_status
            .as_ref()
            .is_some_and(|pending| now >= pending.due_at);
        if !due {
            return false;
        }
        match self.pending_status.take().map(|pending| pending.change) {
            Some(PendingChange::Status(status)) => self.status = status,
            Some(PendingChange::Removal) => return true,
            // Unreachable: the change was there a line ago, under this one `&mut self`.
            // Answered as "nothing was due" rather than with a panic in a request path.
            None => {}
        }
        false
    }

    /// `DBProxyEndpointArn`, from the stored region and name.
    pub fn arn(&self) -> String {
        db_proxy_endpoint_arn(&self.region, &self.name)
    }

    /// Whether this endpoint serves READ-ONLY operations: "a value that indicates whether
    /// the DB proxy endpoint can be used for read/write or read-only operations"
    /// (`DBProxyEndpoint$TargetRole`), which is what a session arriving on its address is
    /// admitted under (`super::data_plane::proxy_endpoint`).
    pub fn serves_reads_only(&self) -> bool {
        self.target_role
            .eq_ignore_ascii_case(DB_PROXY_ENDPOINT_TARGET_ROLE_READ_ONLY)
    }

    /// `Endpoint` as this endpoint publishes it: the address of its OWN listener where this
    /// emulator opened one, and otherwise the DNS name RDS gives a proxy endpoint —
    /// `<endpoint name>.endpoint.proxy-<the proxy's server-assigned label>.<region>` under
    /// the RDS endpoint domain (Amazon RDS User Guide, "Working with RDS Proxy endpoints"),
    /// so a created endpoint sits beside the address the proxy itself publishes
    /// ([`DbProxyRecord::endpoint`]) and is announced in the same zone.
    ///
    /// The served form carries the PORT the core assigned that listener, because
    /// `DBProxyEndpoint` models no port member of its own and the core owns the port range —
    /// so a client that reads this string has the whole address it needs, exactly as the
    /// proxy's own `Endpoint` carries one.
    ///
    /// An endpoint whose proxy this store no longer holds and that has no listener of its own
    /// has no address at all: the empty value is what the caller leaves the member out for,
    /// rather than an invented name. A proxy's endpoints leave the store with the proxy, so
    /// that is a state no read reaches.
    fn address(&self, proxy: Option<&DbProxyRecord>) -> String {
        if let Some(served) = &self.served_endpoint {
            return ::service_common::data_plane_addressing::host_port_authority(
                &served.host,
                u16::try_from(served.port).unwrap_or_default(),
            );
        }
        proxy
            .map(|proxy| {
                format!(
                    "{}.endpoint.proxy-{}.{}.rds.amazonaws.com",
                    self.name, proxy.endpoint_host, proxy.region
                )
            })
            .unwrap_or_default()
    }

    /// Project the stored endpoint onto the modelled `DBProxyEndpoint` wire shape — the
    /// output of `CreateDBProxyEndpoint` and one entry of the `DescribeDBProxyEndpoints`
    /// report, so neither can disagree about an endpoint.
    ///
    /// `proxy` is the proxy record this endpoint belongs to, which is where its address comes
    /// from ([`Self::address`]) and what decides the status it reports: a proxy whose delete
    /// was accepted takes its endpoints with it, so they report
    /// [`DB_PROXY_ENDPOINT_STATUS_DELETING`] while it does, and otherwise an endpoint reports
    /// what it is doing itself. `IsDefault` is `false`: this endpoint exists because a caller
    /// created it, and "RDS automatically creates one endpoint for each DB proxy" — that
    /// other one ([`default_proxy_endpoint`]) is the default.
    pub fn to_wire(&self, proxy: Option<&DbProxyRecord>) -> DBProxyEndpoint {
        let status = match proxy {
            Some(proxy) if proxy.is_deleting() => DB_PROXY_ENDPOINT_STATUS_DELETING.to_string(),
            _ => self.status.clone(),
        };
        DBProxyEndpoint {
            db_proxy_endpoint_name: Some(self.name.clone()),
            db_proxy_endpoint_arn: Some(self.arn()),
            db_proxy_name: Some(self.db_proxy_name.clone()),
            status: Some(status),
            vpc_id: self.vpc_id.clone(),
            vpc_security_group_ids: Some(self.vpc_security_group_ids.clone()),
            vpc_subnet_ids: Some(self.vpc_subnet_ids.clone()),
            endpoint: Some(self.address(proxy)).filter(|address| !address.is_empty()),
            created_date: Some(self.created_date),
            target_role: Some(self.target_role.clone()),
            is_default: Some(false),
            endpoint_network_type: Some(self.endpoint_network_type.clone()),
        }
    }
}

/// The endpoint `proxy` publishes without anybody creating one: the DEFAULT endpoint RDS
/// creates with every DB proxy.
///
/// It carries the proxy's own name, because the default endpoint is the proxy's own address
/// rather than one a caller named, and it is READ/WRITE and `IsDefault` by what
/// `DBProxyEndpoint$IsDefault` documents. Its address is exactly the one the proxy publishes
/// ([`DbProxyRecord::endpoint`]) — a client connecting to the proxy's default endpoint
/// connects to the proxy — and its network, VPC, subnets, security groups and creation
/// instant are the proxy's, since RDS created the two together.
pub fn default_proxy_endpoint(proxy: &DbProxyRecord) -> DBProxyEndpoint {
    DBProxyEndpoint {
        db_proxy_endpoint_name: Some(proxy.name.clone()),
        db_proxy_endpoint_arn: Some(db_proxy_endpoint_arn(&proxy.region, &proxy.name)),
        db_proxy_name: Some(proxy.name.clone()),
        status: Some(proxy_endpoint_status(&proxy.status).to_string()),
        vpc_id: proxy.vpc_id.clone(),
        vpc_security_group_ids: Some(proxy.vpc_security_group_ids.clone()),
        vpc_subnet_ids: Some(proxy.vpc_subnet_ids.clone()),
        endpoint: Some(proxy.endpoint()),
        created_date: Some(proxy.created_date),
        target_role: Some(DB_PROXY_ENDPOINT_TARGET_ROLE_READ_WRITE.to_string()),
        is_default: Some(true),
        endpoint_network_type: Some(proxy.endpoint_network_type.clone()),
    }
}
