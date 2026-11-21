// SPDX-License-Identifier: CC0-1.0

//! The VPC ORIGIN slice of this service's state.
//!
//! "An Amazon CloudFront VPC origin" is its own resource kind — addressed by a minted `Id`,
//! carrying the endpoint configuration it was created with — so everything about how one is
//! persisted and projected lives here rather than in the parent module: the record, the id
//! and ARN one is addressed by, the lookup every read goes through, and the modelled errors
//! the VPC origin operations answer with. The parent
//! [`ComAmazonawsCloudfrontCloudfront20200531State`] holds the one map (`vpc_origins`) and
//! this module holds everything that reads or writes it.
//!
//! One thing is owed by the CLOCK: a VPC origin CloudFront has just created reports
//! [`VPC_ORIGIN_STATUS_DEPLOYING`] until its endpoint has been deployed to the edge
//! locations, and [`STATUS_DEPLOYED`] after ([`super::PROPAGATION_DELAY`], the span every
//! other propagation of this service is owed on). [`StoredVpcOrigin::settle`] applies it,
//! and nothing ever sleeps for it.

use super::*;
use crate::ops::types::{VpcOrigin, VpcOriginEndpointConfig, VpcOriginSummary};

/// The prefix every VPC origin id this service mints carries.
///
/// The model states no pattern for `VpcOrigin$Id`, but its own examples spell a VPC origin
/// id `vo_` followed by a token body, so a minted id carries that namespace label exactly as
/// a distribution tenant's ([`DISTRIBUTION_TENANT_ID_PREFIX`]) and a trust store's
/// ([`TRUST_STORE_ID_PREFIX`]) carry theirs. The body is this mint's
/// ([`ComAmazonawsCloudfrontCloudfront20200531State::allocate_vpc_origin_id`]).
pub const VPC_ORIGIN_ID_PREFIX: &str = "vo_";

/// `VpcOrigin$Status` while the VPC origin's endpoint is still being deployed to the edge
/// locations — the status the model's own `CreateVpcOrigin` example answers with, before the
/// terminal [`STATUS_DEPLOYED`] its read examples report.
///
/// A VPC origin CloudFront has just accepted is not serving origin requests yet, so the
/// create answers this non-terminal status and the clock — not a later call — owes it
/// [`STATUS_DEPLOYED`].
pub const VPC_ORIGIN_STATUS_DEPLOYING: &str = "Deploying";

/// The most VPC origins this emulator holds.
///
/// `CreateVpcOrigin` models `EntityLimitExceeded` ("The entity limit has been exceeded"), so
/// the store is bounded; the bound is this emulator's, CHOSEN like [`MAX_TRUST_STORES`]
/// rather than read off a published quota, and it is an entry count refused before anything
/// is minted.
pub const MAX_VPC_ORIGINS: usize = 1_000;

/// The sentence CloudFront answers an id no VPC origin answers to with.
///
/// It states the CONDITION and echoes no part of the request, as every other not-found
/// sentence of this service states its own ([`TRUST_STORE_NOT_FOUND_MESSAGE`]), so every
/// operation that resolves a VPC origin answers an id it holds none under the same way. It is
/// the answer to a READABLE identifier alone ([`is_vpc_origin_id`]): an argument of no form a
/// VPC origin id takes never named a VPC origin to be absent.
///
/// The value below is the WIRE text — what the recorded service answers this condition with for
/// THIS resource kind, down to the lower-case resource name and the absent full stop, which is
/// not how the neighbouring kinds write their own sentence: do not "correct" it into theirs.
pub const VPC_ORIGIN_NOT_FOUND_MESSAGE: &str = "The specified vpc origin does not exist";

/// The modelled `EntityNotFound` (HTTP 404) for an `Id` no stored VPC origin answers to.
///
/// Takes no id: the sentence is fixed ([`VPC_ORIGIN_NOT_FOUND_MESSAGE`]), so every operation
/// that resolves a VPC origin refuses this condition the same way.
///
/// This is the answer a READABLE VPC origin id ([`is_vpc_origin_id`]) the store holds nothing
/// under gets; a value of no form a VPC origin id is written in is the argument this service
/// cannot read at all ([`invalid_id_format`]), which is a different condition and a different
/// code.
pub fn vpc_origin_not_found() -> EntityNotFound {
    EntityNotFound {
        message: Some(VPC_ORIGIN_NOT_FOUND_MESSAGE.to_string()),
    }
}

/// The sentence CloudFront answers a VPC origin name that is already taken with, on the wire.
///
/// `CreateVpcOrigin` models `EntityAlreadyExists` for the one value a caller CHOOSES about a
/// VPC origin — `VpcOriginEndpointConfig$Name` — so the conflict this sentence states is a name
/// the account already holds. The WIRE text for THIS resource kind, fixed whole: what the
/// refusal states is a property of the CONDITION, so neither the name submitted nor the
/// identifier of the resource holding it reaches it. It differs from the sentence the MODEL's
/// documentation of the shape uses ("The entity already exists. You must provide a unique
/// entity."), which documents the condition for every kind sharing the code and is not served,
/// and from the way the neighbouring kinds that share the code word their own
/// ([`TRUST_STORE_ALREADY_EXISTS_MESSAGE`]).
pub const VPC_ORIGIN_ALREADY_EXISTS_MESSAGE: &str =
    "Another vpc origin with the same name already exists within the aws account.";

/// The modelled `EntityAlreadyExists` (HTTP 409) for a VPC origin name that is already taken.
///
/// Takes no name, for the reason [`trust_store_already_exists`] takes none: every refusal of
/// this condition reads the same ([`VPC_ORIGIN_ALREADY_EXISTS_MESSAGE`]).
pub fn vpc_origin_already_exists() -> EntityAlreadyExists {
    EntityAlreadyExists {
        message: Some(VPC_ORIGIN_ALREADY_EXISTS_MESSAGE.to_string()),
    }
}

/// The sentence a delete refuses a VPC origin whose endpoint has not been deployed yet with.
///
/// `DeleteVpcOrigin` models `IllegalDelete` ("Deletion is not allowed for this entity"), and the
/// condition it states for this resource kind is the one the CLOCK owes away: a VPC origin whose
/// create is still propagating to the edge locations ([`VPC_ORIGIN_STATUS_DEPLOYING`]) is not a
/// VPC origin a delete may remove, so the refusal stands until the deployment settles. Worded in
/// the form [`ILLEGAL_DELETE_MANAGED_POLICY_MESSAGE`] gives this code — the CONDITION and
/// nothing from the request — so every refusal of it reads the same.
pub const VPC_ORIGIN_ILLEGAL_DELETE_MESSAGE: &str =
    "The request failed because delete is not allowed while the VPC origin is being deployed.";

/// The modelled `IllegalDelete` (HTTP 400) for a delete of a VPC origin that is still deploying
/// ([`VPC_ORIGIN_ILLEGAL_DELETE_MESSAGE`]).
pub fn vpc_origin_illegal_delete() -> crate::ops::types::IllegalDelete {
    crate::ops::types::IllegalDelete {
        message: Some(VPC_ORIGIN_ILLEGAL_DELETE_MESSAGE.to_string()),
    }
}

/// Whether `value` is written the way CloudFront writes a VPC origin identifier: the VPC
/// origin namespace label ([`VPC_ORIGIN_ID_PREFIX`]) followed by a body of the width and
/// alphabet this service issues one at ([`is_token_body`], the body every token it mints
/// carries), which is the form the model's own VPC origin examples spell.
///
/// This is ALL the request alone decides about such an identifier, and it is what the
/// operations ADDRESSED by one (`GetVpcOrigin$Id`,
/// `ListDistributionsByVpcOriginId$VpcOriginId`) read their path member with. The SAME split
/// [`unresolved_key_group_id`] makes for another resource of this service, over this kind's
/// own namespace: whether an identifier of this form was ever ISSUED is a property of the
/// STORE, so a value of this form nothing answers to is a VPC origin that is ABSENT
/// ([`vpc_origin_not_found`]), while every other value — another resource kind's identifier,
/// an opaque identifier of the width CloudFront writes ANOTHER kind's body at, this VPC
/// origin's own ARN, an upper-cased label, a bare word — is the argument the service reports
/// it cannot read ([`invalid_id_format`]). Recorded behaviour decides the split, since the
/// model constrains `VpcOrigin$Id` no further than `string`: a `vo_` label carrying a body of
/// another kind's width is answered the invalid-format refusal rather than the absent-origin
/// one.
///
/// Only an identifier that ADDRESSES the operation is judged this way: a VPC origin id borne by
/// a submitted DOCUMENT (`VpcOriginConfig$VpcOriginId` on a distribution configuration)
/// addresses nothing, so it is resolved against the store alone.
pub fn is_vpc_origin_id(value: &str) -> bool {
    value
        .strip_prefix(VPC_ORIGIN_ID_PREFIX)
        .is_some_and(is_token_body)
}

/// The path of the `VpcOriginEndpointConfig$Arn` member, as a refusal naming it spells it
/// ([`parameter_validation_error`]): the enclosing shape and the member, separated by a space.
pub const VPC_ORIGIN_ENDPOINT_CONFIG_ARN_MEMBER_PATH: &str = "VpcOriginEndpointConfig Arn";

/// The ARN service namespace of the load balancers a VPC origin endpoint can front.
pub const LOAD_BALANCER_ARN_NAMESPACE: &str = "elasticloadbalancing";

/// The ARN service namespace of the compute instances a VPC origin endpoint can front.
pub const INSTANCE_ARN_NAMESPACE: &str = "ec2";

/// The load balancer TYPE CODES an Elastic Load Balancing ARN spells for the two kinds of load
/// balancer a VPC origin endpoint can front — an Application Load Balancer (`app`) and a
/// Network Load Balancer (`net`).
///
/// An Elastic Load Balancing ARN spells its resource `loadbalancer/<type code>/<name>/<id>`,
/// and the third code that service mints (`gwy`, a Gateway Load Balancer) is not a kind
/// CloudFront creates a VPC origin for, so it is not in this table.
pub const VPC_ORIGIN_LOAD_BALANCER_TYPE_CODES: [&str; 2] = ["app", "net"];

/// The width of the resource id an Elastic Load Balancing ARN ends with: 16 hexadecimal
/// characters, the id that service mints for every load balancer it creates.
const LOAD_BALANCER_ID_LEN: usize = 16;

/// The widths of the hexadecimal body an EC2 instance id carries after its `i-` label: the
/// eight characters of the short form and the seventeen of the long one.
const INSTANCE_ID_BODY_LENS: [usize; 2] = [8, 17];

/// The longest name a load balancer carries, from `CreateLoadBalancer`'s `Name`
/// documentation: "can have a maximum of 32 characters".
const MAX_LOAD_BALANCER_NAME: usize = 32;

/// The prefix a load balancer name may not begin with, from the same documentation: "must not
/// begin with 'internal-'" — the label Elastic Load Balancing itself puts in front of an
/// internal load balancer's DNS name.
const RESERVED_LOAD_BALANCER_NAME_PREFIX: &str = "internal-";

/// Whether `name` is a name Elastic Load Balancing can have issued a load balancer under.
///
/// Every clause is one of `CreateLoadBalancerInput$Name`'s own: the name "must be unique per
/// region per account, can have a maximum of 32 characters, must contain only alphanumeric
/// characters or hyphens, must not begin or end with a hyphen, and must not begin with
/// 'internal-'". Uniqueness is a property of that service's store and no part of what a
/// value says about itself, so the clauses read here are the ones a value alone answers.
///
/// A segment that breaks one of them is a name no load balancer of that service could ever
/// carry, so an ARN carrying it names nothing — which is what makes it readable as a refusal
/// of the ARN rather than a resource that is merely absent.
fn is_load_balancer_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_LOAD_BALANCER_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.starts_with(RESERVED_LOAD_BALANCER_NAME_PREFIX)
}

/// Whether `value` is an ARN naming a resource a VPC origin endpoint can front.
///
/// `VpcOriginEndpointConfig$Arn` is "The ARN of the CloudFront VPC origin endpoint
/// configuration" — the resource in the caller's VPC the origin sends requests to — and the
/// resources a VPC origin is created for are an Application Load Balancer, a Network Load
/// Balancer and an EC2 instance. So a value that validates is readable as an ARN at all
/// ([`is_readable_arn`]), names a region and an account — such a resource lives in one region
/// and is owned by one account — and spells, in the namespace that owns the kind, the resource
/// form that kind's own ARNs carry:
///
/// * `elasticloadbalancing` → `loadbalancer/<type code>/<name>/<id>`, with a type code of a
///   load balancer CloudFront fronts ([`VPC_ORIGIN_LOAD_BALANCER_TYPE_CODES`]), a name that
///   service can have issued a load balancer under ([`is_load_balancer_name`]), and the 16
///   hexadecimal characters of its resource id;
/// * `ec2` → `instance/i-<hex>`, the instance id form EC2 issues
///   ([`INSTANCE_ID_BODY_LENS`]).
///
/// What it does NOT ask is whether the resource EXISTS: the resource belongs to another
/// service, and this check is the FORM of the value the request carried, decided without
/// asking any store what it holds. What it DOES decide is that the value could name a
/// resource at all — a segment outside the form the owning service issues names one that
/// could never have been created.
pub fn is_vpc_origin_endpoint_arn(value: &str) -> bool {
    if !is_readable_arn(value) {
        return false;
    }
    let segments: Vec<&str> = value.splitn(6, ':').collect();
    let [_, _, service, region, account, resource] = segments[..] else {
        return false;
    };
    !region.is_empty() && !account.is_empty() && names_vpc_origin_resource(service, resource)
}

/// Whether `resource` is the resource part of an ARN of `service` naming a resource a VPC
/// origin endpoint can front — the per-kind forms [`is_vpc_origin_endpoint_arn`] documents.
fn names_vpc_origin_resource(service: &str, resource: &str) -> bool {
    match service {
        LOAD_BALANCER_ARN_NAMESPACE => {
            let Some(rest) = resource.strip_prefix("loadbalancer/") else {
                return false;
            };
            let [type_code, name, id] = rest.split('/').collect::<Vec<&str>>()[..] else {
                return false;
            };
            VPC_ORIGIN_LOAD_BALANCER_TYPE_CODES.contains(&type_code)
                && is_load_balancer_name(name)
                && id.len() == LOAD_BALANCER_ID_LEN
                && id.bytes().all(|b| b.is_ascii_hexdigit())
        }
        INSTANCE_ARN_NAMESPACE => {
            let Some(body) = resource.strip_prefix("instance/i-") else {
                return false;
            };
            INSTANCE_ID_BODY_LENS.contains(&body.len())
                && body.bytes().all(|b| b.is_ascii_hexdigit())
        }
        _ => false,
    }
}

/// The ARN service namespaces that own the resources a VPC origin endpoint can front: the
/// load balancers ([`LOAD_BALANCER_ARN_NAMESPACE`]) and the compute instances
/// ([`INSTANCE_ARN_NAMESPACE`]) `VpcOriginEndpointConfig$Arn` names.
pub const VPC_ORIGIN_ENDPOINT_ARN_NAMESPACES: [&str; 2] =
    [LOAD_BALANCER_ARN_NAMESPACE, INSTANCE_ARN_NAMESPACE];

/// Whether `value` is an ARN of one of the services that own a resource a VPC origin endpoint
/// can front ([`VPC_ORIGIN_ENDPOINT_ARN_NAMESPACES`]) — read from the value alone, with no
/// question asked about the resource it names.
///
/// This is the FORM `VpcOriginEndpointConfig$Arn` takes: the member holds the ARN of a load
/// balancer or of an instance, so a value that is no ARN at all AND one that is an ARN of any
/// other service are both outside that form, while one inside it goes on to be VALIDATED
/// ([`is_vpc_origin_endpoint_arn`]). That split is what the two sentences this member is refused
/// with state ([`vpc_origin_endpoint_config_validation`]).
pub fn is_vpc_origin_endpoint_arn_namespace(value: &str) -> bool {
    if !is_readable_arn(value) {
        return false;
    }
    value
        .split(':')
        .nth(2)
        .is_some_and(|service| VPC_ORIGIN_ENDPOINT_ARN_NAMESPACES.contains(&service))
}

/// The refusal CloudFront's argument validation answers a `VpcOriginEndpointConfig` it does
/// not accept, or `None` when the configuration validates.
///
/// `Arn` is the member that NAMES a resource, and the two ways that value can fail are two
/// refusals, told apart as the service tells them apart:
///
/// * a value outside the FORM the member takes — the member holds the ARN of one of the
///   resources a VPC origin fronts, so a value that is no ARN at all and one that is an ARN of
///   any other service alike are not in that form
///   ([`is_vpc_origin_endpoint_arn_namespace`]) — is refused on the member's form
///   ([`parameter_invalid_format`]);
/// * a value in that form whose resource part is not one that kind's own ARNs carry, so it
///   names no resource a VPC origin can front ([`is_vpc_origin_endpoint_arn`]), fails that
///   member's validation ([`parameter_validation_error`]).
///
/// Either way the member is named by its PATH in the request. Both sentences are recorded
/// behaviour of this member, so the split between them is the member's form against its
/// validation and not one condition worded two ways. The two PORTS are read with this service's
/// one origin-port rule. Nothing is minted from a configuration this refuses.
///
/// The configuration's other members are judged by the model itself: `Name`, `Arn`,
/// `HTTPPort`, `HTTPSPort` and `OriginProtocolPolicy` are `@required` and the protocol members
/// are enumerations, so a request missing one or naming a value outside one never reaches a
/// handler.
///
/// One walk for every operation that takes an endpoint configuration, so a create and a later
/// update cannot disagree about which configurations this service will store.
pub fn vpc_origin_endpoint_config_validation(
    config: &VpcOriginEndpointConfig,
) -> Option<crate::ops::types::InvalidArgument> {
    if !is_vpc_origin_endpoint_arn(&config.arn) {
        // One reading of the value decides which of the two refusals it gets: a value outside
        // the form this member takes — no ARN at all, or an ARN of a service that owns no
        // resource a VPC origin fronts — failed the member's FORM, and one inside that form
        // whose resource part names no such resource failed its validation.
        return Some(if is_vpc_origin_endpoint_arn_namespace(&config.arn) {
            parameter_validation_error(VPC_ORIGIN_ENDPOINT_CONFIG_ARN_MEMBER_PATH)
        } else {
            parameter_invalid_format(VPC_ORIGIN_ENDPOINT_CONFIG_ARN_MEMBER_PATH)
        });
    }
    // "The HTTP port for the CloudFront VPC origin endpoint configuration" and its HTTPS
    // counterpart are ports an ORIGIN is reached on, so they are read with the one rule this
    // service states for an origin's ports ([`is_origin_port`]) — the same rule a
    // distribution's custom origin is read with, worded the same way.
    [config.http_port, config.https_port]
        .into_iter()
        .any(|port| !is_origin_port(port))
        .then(origin_port_out_of_range)
}

/// The modelled `InconsistentQuantities` message for the one list wrapper a
/// `VpcOriginEndpointConfig` reaches — its `OriginSslProtocols` — or `None` when the two
/// agree.
///
/// The SAME rule [`config_quantity_mismatch`] applies to a distribution configuration: every
/// CloudFront list travels in a wrapper stating its own length (`Quantity`) beside the list
/// itself (`Items`), and the service refuses a request where the two disagree — through the
/// parent module's one `quantity_disagreement` sentence, so no two of these walks can word
/// the refusal differently.
pub fn vpc_origin_endpoint_config_quantity_mismatch(
    config: &VpcOriginEndpointConfig,
) -> Option<String> {
    let protocols = config.origin_ssl_protocols.as_ref()?;
    quantity_disagreement(
        "OriginSslProtocols",
        protocols.quantity,
        protocols.items.len(),
    )
}

/// Whether `config` fronts the VPC origin `id` names through one of its origins.
///
/// An origin reaches a VPC origin through `Origin$VpcOriginConfig`, whose required
/// `VpcOriginId` is "The VPC origin ID" — so a distribution is associated with the VPC origin
/// exactly when ONE of the origins of its stored configuration names it there, and a
/// configuration whose origins all describe an S3 or custom origin is associated with no VPC
/// origin at all. Read from the STORED configuration, like every other association predicate
/// of this store ([`references_connection_function`], [`references_key_group`]), so the
/// listing of the distributions a VPC origin fronts can never disagree with what a read of
/// those distributions reports. An empty `id` names no VPC origin — none is ever minted — so
/// it matches nothing.
pub fn references_vpc_origin(config: &DistributionConfig, id: &str) -> bool {
    if id.is_empty() {
        return false;
    }
    config
        .origins
        .items
        .iter()
        .filter_map(|origin| origin.vpc_origin_config.as_ref())
        .any(|vpc| vpc.vpc_origin_id == id)
}

/// One CloudFront VPC origin, as this emulator persists it.
///
/// The record is the single source of truth for the VPC origin's id, ARN, status, the
/// endpoint configuration it was created with, the instants it was created and last written,
/// its version token and its tags — so the create and every later read answer from it rather
/// than from the request being served.
#[derive(Debug, Clone)]
pub struct StoredVpcOrigin {
    /// `VpcOrigin$Id` — "The VPC origin ID": the token minted at creation, which is also the
    /// key this record is held under and the last segment of its ARN.
    pub id: String,
    /// `VpcOrigin$Arn` — "The VPC origin ARN", built from the minted id once
    /// ([`vpc_origin_arn`]) and stored, so every read reports the same ARN.
    pub arn: String,
    /// [`VPC_ORIGIN_STATUS_DEPLOYING`] until [`StoredVpcOrigin::deployed_due`], then
    /// [`STATUS_DEPLOYED`].
    pub status: String,
    /// The instant this VPC origin is owed [`STATUS_DEPLOYED`]. Stored rather than slept for;
    /// every stored VPC origin has one, because every one of them was created by a call.
    pub deployed_due: SystemTime,
    /// `VpcOrigin$CreatedTime` — "The VPC origin created time": the instant the call that
    /// minted this record ran, written once so every later read reports the same one.
    pub created_time: SystemTime,
    /// `VpcOrigin$LastModifiedTime` — "The VPC origin last modified time", written by the
    /// call that last wrote this record.
    pub last_modified_time: SystemTime,
    /// The endpoint configuration as submitted, stored whole: a later read answers from this
    /// record, down to the members the request left out.
    pub endpoint_config: VpcOriginEndpointConfig,
    /// The version token the last mutation handed back, and the single source of truth for
    /// every `If-Match` check against this VPC origin.
    pub etag: String,
    /// The tags submitted with the create, held in the one representation every tagged
    /// resource of this service uses, so the tagging surface reads the very set that was
    /// stored.
    pub tags: TagSet,
}

impl StoredVpcOrigin {
    /// Apply the deployment transition the clock owes at `now`.
    ///
    /// Idempotent and monotonic, like every other settlement in this store: it only ever
    /// moves the non-terminal status to the terminal one, only once the deadline has passed,
    /// so a read that calls it cannot change what a later caller sees.
    pub fn settle(&mut self, now: SystemTime) {
        if self.status == VPC_ORIGIN_STATUS_DEPLOYING && now >= self.deployed_due {
            self.status = STATUS_DEPLOYED.to_string();
        }
    }

    /// The `VpcOrigin` projection every VPC origin operation returns, read from the one
    /// stored record.
    ///
    /// `AccountId` is "The account ID of the Amazon Web Services account that owns the VPC
    /// origin", which is this emulator's own account ([`::engine::emulator_account_id`]) for
    /// every VPC origin it holds: each was created through this control plane.
    pub fn to_vpc_origin(&self) -> VpcOrigin {
        VpcOrigin {
            id: self.id.clone(),
            arn: self.arn.clone(),
            account_id: Some(::engine::emulator_account_id().to_string()),
            status: self.status.clone(),
            created_time: ::aws_smithy_types::DateTime::from(self.created_time),
            last_modified_time: ::aws_smithy_types::DateTime::from(self.last_modified_time),
            vpc_origin_endpoint_config: self.endpoint_config.clone(),
        }
    }

    /// The `VpcOriginSummary` a listing serves for this VPC origin, read from the SAME stored
    /// record the full projection is read from — so a listing and a read of one of its entries
    /// can never disagree.
    ///
    /// The summary is the full projection's members minus the configuration, with the
    /// configuration's own `Name` ("The VPC origin summary name"), its `Arn` ("The VPC origin
    /// summary origin endpoint ARN" — the resource in the VPC this origin fronts, which the
    /// summary carries beside the VPC origin's own ARN) and its `IpAddressType` hoisted onto
    /// it.
    pub fn to_summary(&self) -> VpcOriginSummary {
        VpcOriginSummary {
            id: self.id.clone(),
            name: self.endpoint_config.name.clone(),
            status: self.status.clone(),
            created_time: ::aws_smithy_types::DateTime::from(self.created_time),
            last_modified_time: ::aws_smithy_types::DateTime::from(self.last_modified_time),
            arn: self.arn.clone(),
            account_id: Some(::engine::emulator_account_id().to_string()),
            origin_endpoint_arn: self.endpoint_config.arn.clone(),
            ip_address_type: self.endpoint_config.ip_address_type.clone(),
        }
    }
}

impl ComAmazonawsCloudfrontCloudfront20200531State {
    /// Allocate a VPC origin id no stored VPC origin holds: [`VPC_ORIGIN_ID_PREFIX`] followed
    /// by a minted token, drawn from the same mint as every other id this service generates
    /// so no two are minted the same.
    pub fn allocate_vpc_origin_id(&mut self) -> String {
        loop {
            let candidate = format!("{VPC_ORIGIN_ID_PREFIX}{}", self.mint(ID_BODY_LEN));
            if !self.vpc_origins.contains_key(&candidate) {
                break candidate;
            }
        }
    }

    /// The VPC origin `id` names, or `None` when the store holds none.
    ///
    /// Every VPC origin operation addresses one by "The VPC origin ID", which is the key this
    /// record is held under, so the lookup is that one map read rather than a scan.
    pub fn vpc_origin(&self, id: &str) -> Option<&StoredVpcOrigin> {
        self.vpc_origins.get(id)
    }

    /// Whether the store holds a VPC origin under exactly the ID `id` — what tells a `Marker`
    /// this service handed out from one shaped like an id that names no VPC origin, which is a
    /// resume point that resumes nothing rather than a bad argument.
    ///
    /// By KEY rather than through [`Self::vpc_origin`]: a resume point is the id the previous
    /// page ended at.
    pub fn holds_vpc_origin(&self, id: &str) -> bool {
        self.vpc_origins.contains_key(id)
    }

    /// Whether the account already holds a VPC origin whose endpoint configuration is called
    /// `name` — what makes a repeated name a [`vpc_origin_already_exists`] rather than a second
    /// VPC origin.
    ///
    /// A scan over the stored records rather than a second index beside them, for the reason
    /// [`ComAmazonawsCloudfrontCloudfront20200531State::holds_trust_store_named`] scans: those
    /// records are the single source of truth for which names are taken, so the answer cannot
    /// drift out of step with what a read reports, and the collection is bounded by
    /// [`MAX_VPC_ORIGINS`].
    pub fn holds_vpc_origin_named(&self, name: &str) -> bool {
        self.vpc_origins
            .values()
            .any(|vpc_origin| vpc_origin.endpoint_config.name == name)
    }

    /// The id of the distribution one of whose origins fronts the VPC origin `id` names, or
    /// `None` when nothing stored does.
    ///
    /// What makes a VPC origin IN USE, and so what a delete of one is refused for: a
    /// distribution names a VPC origin in exactly one place ([`references_vpc_origin`]), the
    /// same place the listing of the distributions a VPC origin fronts reads, so the two can
    /// never disagree about which VPC origins are still referenced. A scan over the STORED
    /// configurations rather than a second index beside them, for the reason
    /// [`ComAmazonawsCloudfrontCloudfront20200531State::distribution_using_trust_store`] scans.
    pub fn distribution_using_vpc_origin(&self, id: &str) -> Option<&str> {
        self.distributions
            .values()
            .find(|stored| references_vpc_origin(&stored.config, id))
            .map(|stored| stored.id.as_str())
    }
}
