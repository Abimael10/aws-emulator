// SPDX-License-Identifier: CC0-1.0

//! Engine state for the com-amazonaws-supplychain-galaxypublicapigateway service.
//!
//! The AWS Supply Chain **Instance** is this service's primary resource: every other
//! resource (data-lake namespaces and datasets, data-integration flows and events) is
//! addressed by an `instanceId`, so the instance store lives here and the per-operation
//! handlers in `src/ops/` read and write it through [`service_state`].

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, SystemTime};

use crate::ops::types::{
    DataIntegrationEvent, DataIntegrationEventDatasetTargetDetails, DataIntegrationFlow,
    DataIntegrationFlowSource, DataIntegrationFlowTarget, DataIntegrationFlowTransformation,
    DataLakeDataset, DataLakeDatasetPartitionSpec, DataLakeDatasetSchema, DataLakeNamespace,
    Instance,
};

/// The sentence AWS Supply Chain refuses the Product Bill Of Materials (BOM) import API
/// with: that API is WITHDRAWN, so both of its operations
/// (`CreateBillOfMaterialsImportJob` and `GetBillOfMaterialsImportJob`) answer the
/// modelled `AccessDeniedException` (HTTP 403) carrying it.
///
/// Its SOURCE is the recorded wire response of this API — the recorded unit
/// `supplychain.bom-import-job-fails-without-s3-access` probes both halves of it and both
/// answer this sentence. The model declares no deprecation and fixes no text, and the
/// operations' own documentation still describes creating and reading an import job, so a
/// later reader re-verifies this against a recording rather than against the model. The
/// whole sentence is FIXED: it names the API that has been withdrawn and the channel to
/// ask about it, and carries nothing from the request, so no part of it is interpolated.
/// Stated once, here, so the two operations cannot drift into two sentences for one
/// refusal.
pub const WITHDRAWN_BOM_IMPORT_API_MESSAGE: &str = "This API has been deprecated. Please contact \
                                                    ASC support team at \
                                                    https://aws.amazon.com/contact-us/ for more \
                                                    details.";

/// `InstanceState` as the model spells it: a freshly created instance is still being
/// built. `CreateInstance`'s own documentation — "Upon receiving a CreateInstance
/// request, Amazon Web Services Supply Chain immediately returns the instance resource,
/// instance ID, and the initializing state" — is what pins this as the initial state.
pub const INSTANCE_STATE_INITIALIZING: &str = "Initializing";

/// `InstanceState` for an instance whose creation finished: the terminal healthy state
/// a client polling `GetInstance` for readiness waits for.
pub const INSTANCE_STATE_ACTIVE: &str = "Active";

/// `InstanceState` for an instance whose deletion has been accepted.
pub const INSTANCE_STATE_DELETING: &str = "Deleting";

/// How long after `CreateInstance` the clock owes an instance its
/// `Initializing` -> `Active` transition.
///
/// CHOSEN, not documented: AWS states only that `CreateInstance` is asynchronous and
/// returns `Initializing`, never how long initialization takes. The span is deliberately
/// non-zero so a caller that reads the instance back immediately still observes
/// `Initializing` (and so a readiness poll has something to converge on), and short
/// enough that the poll terminates promptly.
pub const INSTANCE_ACTIVATION_DELAY: Duration = Duration::from_millis(500);

/// The `versionNumber` an instance of this service reports.
///
/// RECORDED behaviour, not modelled: `Instance$versionNumber` is documented only as "The
/// version number of the instance" and the model says nothing about how it moves. In the
/// recorded wire responses it is a property OF THE INSTANCE rather than a count of the
/// writes made to it — one instance reports the same number from its create, from every
/// later read, from an update that changed its name or description, and from its delete —
/// so it names the generation of the AWS Supply Chain application the instance runs, which
/// nothing a caller does to the instance moves. Seeded onto each record (rather than read
/// from here at projection time) so the stored instance carries its own version.
pub const INSTANCE_VERSION_NUMBER: f64 = 2.0;

/// The DNS zone an instance's WebApp hostname and its IAM Identity Center portal hostname
/// sit under.
///
/// Its SOURCE is the recorded wire response of the instance operations, and the model's own
/// documentation of `webAppDnsDomain` corroborates it — "The DNS subdomain of the web app.
/// This would be \"example\" in the URL \"example.scn.global.on.aws\"" — so the instance's
/// own domain label is the only variable part of either hostname and this suffix is fixed
/// text.
pub const WEB_APP_ZONE: &str = "scn.global.on.aws";

/// The hostname an instance's domain `label` resolves to: the label followed by the
/// service's WebApp zone ([`WEB_APP_ZONE`]). Stated once, here, so the hostnames the
/// `Instance` projection reports cannot drift from each other.
pub fn web_app_zone_hostname(label: &str) -> String {
    format!("{label}.{WEB_APP_ZONE}")
}

/// The `webAppDnsDomain` an instance gets when its create request named none: AWS answers
/// such a create with an instance that still has a WebApp domain — the service generates
/// the label — so one is generated here rather than left absent. The generated VALUE is the
/// emulator's own (AWS's is a per-run server-generated token, so no value reproduces it);
/// what it has to satisfy is the member's modelled form and its invariants.
///
/// The instance's own id is the label: it satisfies `InstanceWebAppDnsDomain`'s modelled
/// pattern (`^(?![-])[a-zA-Z0-9-]{1,62}[a-zA-Z0-9]$` — 36 characters of lowercase hex and
/// hyphens, starting and ending alphanumeric), it is stable for the life of the instance,
/// and no other instance can hold it, so a generated domain never collides with the domain
/// another live instance holds.
pub fn generated_web_app_dns_domain(instance_id: &str) -> String {
    instance_id.to_string()
}

/// The number of instances this store holds.
///
/// A generated emulator keeps its resources in memory for the life of the process and
/// `CreateInstance` is the only thing that adds one, so the store needs a maximum entry
/// COUNT: neither a TTL nor an eviction policy is available to it, because `GetInstance`
/// must still answer what `CreateInstance` created. Each record's size is already bounded
/// by the modelled `@length` constraints on its members, so the count is the remaining
/// growth term, and a create past it is refused with the service's own modelled
/// `ServiceQuotaExceededException` (HTTP 402) — which `CreateInstance` declares and no
/// read does. The number is the emulator's own bound, not a published AWS value: AWS
/// quotas instances per account as an ADJUSTABLE limit, so no single number is the
/// contract, and this one is far above any ordinary workflow.
pub const MAX_INSTANCES: usize = 100;

/// The number of `clientToken` entries the idempotency table holds.
///
/// Each token names the one instance it created and `DeleteInstance` drops the tokens
/// naming a record it removes, so the table never holds more entries than there are live
/// instances — but the keys are caller-supplied strings, so the bound is enforced at the
/// one growth site rather than left to that invariant. Same cap, same refusal as
/// [`MAX_INSTANCES`].
pub const MAX_IDEMPOTENCY_TOKENS: usize = MAX_INSTANCES;

/// How many v4 UUIDs [`ComAmazonawsSupplychainGalaxypublicapigatewayState::fresh_instance_id`]
/// draws before giving up.
///
/// A bound rather than a retry loop: with at most [`MAX_INSTANCES`] live ids, a single
/// draw colliding is already vanishingly unlikely, so exhausting these attempts means
/// something is wrong with the generator — which the caller reports as a server error
/// instead of spinning while holding the state lock.
pub const ID_MINT_ATTEMPTS: usize = 8;

/// One persisted AWS Supply Chain instance — the whole record, not the wire shape.
/// [`InstanceRecord::to_wire`] projects it onto the modelled `Instance` structure, so
/// every handler answers from exactly these stored values.
#[derive(Debug, Clone)]
pub struct InstanceRecord {
    pub instance_id: String,
    pub aws_account_id: String,
    pub state: String,
    pub error_message: Option<String>,
    pub instance_name: Option<String>,
    pub instance_description: Option<String>,
    pub kms_key_arn: Option<String>,
    pub web_app_dns_domain: Option<String>,
    pub created_time: SystemTime,
    pub last_modified_time: SystemTime,
    pub version_number: f64,
    pub tags: BTreeMap<String, String>,
    /// Where this instance falls in the order the store's instances were created, as
    /// [`ComAmazonawsSupplychainGalaxypublicapigatewayState::next_instance_creation_seq`]
    /// handed it out: strictly increasing, so a larger value is a later create. Kept on
    /// the record because `ListInstances` reports the most recently created instance
    /// first, an order neither the store's key nor a wall-clock stamp (two creates can
    /// share one) gives.
    pub creation_seq: u64,
    /// The instant at which this instance's `Initializing` -> `Active` transition
    /// becomes due. Stored rather than slept on: the transition is materialised by
    /// whichever call next observes the record (see
    /// [`ComAmazonawsSupplychainGalaxypublicapigatewayState::materialize_due_transitions`]).
    pub active_due: SystemTime,
}

impl InstanceRecord {
    /// A brand-new instance in `Initializing`, owned by the emulator's account.
    ///
    /// The optional members (`instanceName`, `instanceDescription`, `kmsKeyArn`,
    /// `webAppDnsDomain`, `tags`) are left absent for the caller to fill from the
    /// request, so a member the request omitted stays absent rather than being
    /// invented — except `webAppDnsDomain`, which every instance has: the create either
    /// echoes the one the request named or generates one
    /// ([`generated_web_app_dns_domain`]).
    pub fn new(instance_id: String, now: SystemTime, creation_seq: u64) -> Self {
        Self {
            instance_id,
            aws_account_id: ::engine::emulator_account_id().to_string(),
            state: INSTANCE_STATE_INITIALIZING.to_string(),
            error_message: None,
            instance_name: None,
            instance_description: None,
            kms_key_arn: None,
            web_app_dns_domain: None,
            created_time: now,
            last_modified_time: now,
            version_number: INSTANCE_VERSION_NUMBER,
            tags: BTreeMap::new(),
            creation_seq,
            active_due: now + INSTANCE_ACTIVATION_DELAY,
        }
    }

    /// This record as the `Instance` shape every instance operation returns.
    ///
    /// `primaryDomain` and `idcPortalDomain` are the instance's WebApp hostname and its
    /// IAM Identity Center portal hostname: both are the instance's own `webAppDnsDomain`
    /// under the service's WebApp zone ([`web_app_zone_hostname`]), so an instance with no
    /// domain label reports neither rather than a bare zone.
    pub fn to_wire(&self) -> Instance {
        let hostname = self
            .web_app_dns_domain
            .as_deref()
            .map(web_app_zone_hostname);
        Instance {
            instance_id: self.instance_id.clone(),
            aws_account_id: self.aws_account_id.clone(),
            state: self.state.clone(),
            error_message: self.error_message.clone(),
            web_app_dns_domain: self.web_app_dns_domain.clone(),
            created_time: Some(as_date_time(self.created_time)),
            last_modified_time: Some(as_date_time(self.last_modified_time)),
            instance_name: self.instance_name.clone(),
            instance_description: self.instance_description.clone(),
            kms_key_arn: self.kms_key_arn.clone(),
            version_number: Some(self.version_number),
            primary_domain: hostname.clone(),
            idc_portal_domain: hostname,
        }
    }

    /// Record a change made at `now`: a mutation moves `lastModifiedTime` and nothing
    /// else. `versionNumber` deliberately does NOT move — it is the instance's own
    /// version ([`INSTANCE_VERSION_NUMBER`]), not a count of the writes it has taken.
    pub fn touch(&mut self, now: SystemTime) {
        self.last_modified_time = now;
    }
}

/// A `SystemTime` as the modelled timestamp type.
fn as_date_time(time: SystemTime) -> ::aws_smithy_types::DateTime {
    ::aws_smithy_types::DateTime::from(time)
}

/// The ARN namespace every AWS Supply Chain resource carries: `AscResourceArn`'s
/// modelled pattern fixes the prefix `arn:aws:scn`.
pub const ARN_SERVICE: &str = "scn";

/// The region a request that carries no SigV4 credential scope is served in.
pub const DEFAULT_REGION: &str = "us-east-1";

/// The region the caller addressed, read from the request's SigV4 credential scope, so a
/// resource's ARN names the region the call that created it was signed for. Falls back to
/// [`DEFAULT_REGION`] for a request with no scope to read.
pub fn request_region(ctx: &smithy_server_runtime::OperationContext<'_>) -> String {
    ::emulator_server::sigv4_credential_scope(&ctx.parts().headers)
        .map_or_else(|| DEFAULT_REGION.to_string(), |scope| scope.region)
}

/// The ARN of one data lake namespace.
///
/// The modelled `AscResourceArn` pattern
/// (`^arn:aws:scn(?::([a-z0-9-]+):([0-9]+):instance)?/(<uuid>)[-_./A-Za-z0-9]*$`) is what
/// fixes the shape: the instance segment carries the `instanceId`, and everything after
/// it names the resource within that instance.
pub fn namespace_arn(region: &str, instance_id: &str, name: &str) -> String {
    ::engine::emulator_arn(
        ARN_SERVICE,
        region,
        &format!("instance/{instance_id}/namespaces/{name}"),
    )
}

/// How this service's gateway refuses an instance-scoped call whose `instanceId` names
/// no instance of this account.
///
/// Its SOURCE is the recorded wire response of such a call: the gateway authorizes
/// against the instance resource BEFORE looking the addressed resource up, so a request
/// naming an unknown instance is answered as an authorization failure (HTTP 403
/// `AccessDeniedException`) rather than as a not-found. The whole sentence is fixed
/// except the `instanceId`, which comes from the request, and the account id, which is
/// the identity the request was served for. Stated once, here, so the instance-scoped
/// operations cannot drift into several sentences for one refusal.
pub fn instance_authorization_failure_message(instance_id: &str) -> String {
    format!(
        "Authorization Failed (Public). The instanceId {instance_id} does not exist, or does not \
         belong to the account {}",
        ::engine::emulator_account_id()
    )
}

/// How a namespace read refuses a name this instance's data lake does not hold.
///
/// Its SOURCE is the recorded wire response of this service's not-found refusals, which
/// carry a FIXED sentence and echo nothing from the request (the instance reads answer
/// "Instance not found." the same way). Stated once, here, so the namespace operations
/// cannot drift into several sentences for one refusal.
pub const NAMESPACE_NOT_FOUND_MESSAGE: &str = "Namespace not found.";

/// The namespace-name prefixes AWS Supply Chain keeps for itself.
///
/// SOURCE: the `name` member of `CreateDataLakeNamespaceRequest`, which documents
/// "Noted you cannot create namespace with name starting with asc, default, scn, aws,
/// amazon, amzn". `asc` and `default` are the data lake's own pre-defined namespaces
/// (the `name` member of `GetDataLakeNamespaceRequest` describes both), and the rest are
/// the vendor prefixes the service reserves.
pub const RESERVED_NAMESPACE_PREFIXES: [&str; 6] =
    ["asc", "default", "scn", "aws", "amazon", "amzn"];

/// Whether `name` starts with one of the prefixes the service reserves. The rule is on the
/// PREFIX, not on the whole name, so `aws_orders` is refused just as `aws` is.
///
/// The comparison is byte-exact because `DataLakeNamespaceName`'s own `@pattern`
/// (`^[a-z0-9_]+$`) admits no uppercase: a name carrying any is refused by the modelled
/// constraint before a handler runs, so there is no mixed-case form left for this rule to
/// miss.
pub fn namespace_name_is_reserved(name: &str) -> bool {
    RESERVED_NAMESPACE_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// The sentence a namespace create carrying a reserved name is refused with, built from
/// [`RESERVED_NAMESPACE_PREFIXES`] so the refusal and the rule cannot drift apart.
pub fn reserved_namespace_name_message() -> String {
    format!(
        "Cannot create namespace with name starting with {}",
        RESERVED_NAMESPACE_PREFIXES.join(", ")
    )
}

/// How a namespace create refuses a name this instance's data lake already holds.
///
/// The recorded behaviour pins the CLASS of this refusal — a duplicate instance and name is
/// answered `ConflictException` (HTTP 409) rather than overwritten. No recording pins this
/// sentence, so its FORM is taken from the one this service IS recorded wording for a
/// duplicate data-lake resource, the sibling dataset conflict ("Dataset <name> already exist
/// in namespace <namespace>"): the capitalised resource noun, the resource's own name from
/// the request, and the service's "already exist". Stated once, beside the service's other
/// refusal sentences, rather than inlined in the handler.
pub fn namespace_conflict_message(name: &str) -> String {
    format!("Namespace {name} already exist")
}

/// How many namespaces one instance's data lake holds in this store.
///
/// The same reasoning as [`MAX_INSTANCES`]: the store keeps every namespace for the life
/// of the process and the create is the only thing that adds one, so the entry COUNT is
/// the growth term a caller controls (each record's size is already bounded by the
/// modelled `@length` constraints on its members). A create past it is refused with the
/// modelled `ServiceQuotaExceededException` (HTTP 402) the create declares. The number is
/// the emulator's own bound, not a published AWS value.
pub const MAX_NAMESPACES_PER_INSTANCE: usize = 100;

/// One persisted data lake namespace — the whole record, not the wire shape.
/// [`DataLakeNamespaceRecord::to_wire`] projects it onto the modelled `DataLakeNamespace`
/// structure, so every handler answers from exactly these stored values.
#[derive(Debug, Clone)]
pub struct DataLakeNamespaceRecord {
    pub instance_id: String,
    pub name: String,
    pub arn: String,
    pub description: Option<String>,
    pub created_time: SystemTime,
    pub last_modified_time: SystemTime,
    /// The namespace's tags, as the create's `tags` member supplied them. Kept on the
    /// record so the tagging operations report what the create persisted.
    pub tags: BTreeMap<String, String>,
}

impl DataLakeNamespaceRecord {
    /// A brand-new namespace of `instance_id`, created at `now` in `region`.
    ///
    /// `description` and `tags` are left absent/empty for the caller to fill from the
    /// request, so a member the request omitted stays absent rather than being invented.
    pub fn new(instance_id: String, name: String, region: &str, now: SystemTime) -> Self {
        let arn = namespace_arn(region, &instance_id, &name);
        Self {
            instance_id,
            name,
            arn,
            description: None,
            created_time: now,
            last_modified_time: now,
            tags: BTreeMap::new(),
        }
    }

    /// This record as the modelled `DataLakeNamespace` shape every namespace operation
    /// returns.
    pub fn to_wire(&self) -> DataLakeNamespace {
        DataLakeNamespace {
            instance_id: self.instance_id.clone(),
            name: self.name.clone(),
            arn: self.arn.clone(),
            description: self.description.clone(),
            created_time: as_date_time(self.created_time),
            last_modified_time: as_date_time(self.last_modified_time),
        }
    }
}

/// The ARN of one data-lake dataset.
///
/// `AscResourceArn` (the shape `DataLakeDataset::arn` targets) fixes the form: the
/// partition and service are literal, the region and account are the instance ARN's, and
/// everything after the instance id is free `[-_./A-Za-z0-9]*` — so the dataset's
/// namespace and name extend the instance's resource portion along the same path segments
/// the operation's URI uses (`.../namespaces/{namespace}/datasets/{name}`).
pub fn dataset_arn(region: &str, instance_id: &str, namespace: &str, name: &str) -> String {
    ::engine::emulator_arn(
        ARN_SERVICE,
        region,
        &format!("instance/{instance_id}/namespaces/{namespace}/datasets/{name}"),
    )
}

/// The data-lake namespaces an instance has without anybody creating them.
///
/// SOURCE: the `namespace` member's own documentation on the dataset operations — "besides
/// the custom defined namespace, every instance comes with below pre-defined namespaces:
/// asc […] default […]". Every other namespace exists only once it has been created, so a
/// dataset addressed to one that was never created names no namespace at all.
pub const PREDEFINED_NAMESPACES: [&str; 2] = ["asc", "default"];

/// Whether `namespace` is one an instance comes with ([`PREDEFINED_NAMESPACES`]).
pub fn is_predefined_namespace(namespace: &str) -> bool {
    PREDEFINED_NAMESPACES.contains(&namespace)
}

/// How a dataset operation refuses a dataset this instance's data lake does not hold
/// (`ResourceNotFoundException`, HTTP 404).
///
/// Its SOURCE is the recorded wire response of such a read. The FIXED text is
/// "Dataset " and " not exist in namespace " — the service capitalises the resource noun
/// and writes the uninflected "not exist", not "does not exist" — and the only
/// interpolated parts are the dataset name and the namespace name, the two `@httpLabel`
/// members of the dataset URI. The `instanceId`, though part of the resource identity, is
/// NOT part of the sentence. Stated once, here, so the dataset operations cannot drift
/// into several sentences for one refusal.
pub fn dataset_not_found_message(name: &str, namespace: &str) -> String {
    format!("Dataset {name} not exist in namespace {namespace}")
}

/// How a dataset create refuses a `(instanceId, namespace, name)` triple the data lake
/// already holds (`ConflictException`, HTTP 409).
///
/// Its SOURCE is the recorded refusal of a second create for a triple that already holds
/// a dataset: the same shape as [`dataset_not_found_message`] — the capitalised resource
/// noun, the dataset name and its namespace from the request, nothing after the namespace
/// and no instance id — with the service's "already exist" in place of "not exist".
pub fn dataset_conflict_message(name: &str, namespace: &str) -> String {
    format!("Dataset {name} already exist in namespace {namespace}")
}

/// The number of datasets one instance holds, across all of its namespaces.
///
/// Same reasoning as [`MAX_INSTANCES`]: the store keeps its records for the life of the
/// process, each record's size is already bounded by the modelled `@length` constraints
/// on its members, so the entry COUNT is the remaining growth term and it is capped at
/// the one site that adds a dataset — refused with the modelled
/// `ServiceQuotaExceededException` (HTTP 402) that `CreateDataLakeDataset` declares and
/// no read does. The number is the emulator's own bound, not a published AWS value: AWS
/// quotas datasets per instance as an adjustable limit, so no single number is the
/// contract, and this one is far above any ordinary workflow.
pub const MAX_DATASETS_PER_INSTANCE: usize = 200;

/// How a data-lake dataset is addressed: `(instanceId, namespace, name)`, the three
/// `@httpLabel` members every dataset operation's URI carries, in that order.
///
/// A `BTreeMap` keyed by it therefore groups one instance's namespaces, and one
/// namespace's datasets, contiguously and in name order — the order
/// `ListDataLakeDatasets` pages through.
pub type DatasetKey = (String, String, String);

/// The [`DatasetKey`] for one dataset, so every handler builds the key the same way.
pub fn dataset_key(instance_id: &str, namespace: &str, name: &str) -> DatasetKey {
    (
        instance_id.to_string(),
        namespace.to_string(),
        name.to_string(),
    )
}

/// One persisted data-lake dataset — the whole record, not the wire shape.
/// [`DataLakeDatasetRecord::to_wire`] projects it onto the modelled `DataLakeDataset`
/// structure, so every handler answers from exactly these stored values.
#[derive(Debug, Clone)]
pub struct DataLakeDatasetRecord {
    pub instance_id: String,
    pub namespace: String,
    pub name: String,
    pub arn: String,
    pub schema: DataLakeDatasetSchema,
    pub description: Option<String>,
    pub partition_spec: Option<DataLakeDatasetPartitionSpec>,
    pub created_time: SystemTime,
    pub last_modified_time: SystemTime,
    /// The dataset's tags. `tags` is `notProperty` on the request and absent from the
    /// `DataLakeDataset` shape, so they are stored for the tagging operations to report
    /// rather than projected onto the wire shape here.
    pub tags: BTreeMap<String, String>,
}

impl DataLakeDatasetRecord {
    /// This record as the modelled `DataLakeDataset` shape every dataset operation
    /// returns.
    pub fn to_wire(&self) -> DataLakeDataset {
        DataLakeDataset {
            instance_id: self.instance_id.clone(),
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            arn: self.arn.clone(),
            schema: self.schema.clone(),
            description: self.description.clone(),
            partition_spec: self.partition_spec.clone(),
            created_time: as_date_time(self.created_time),
            last_modified_time: as_date_time(self.last_modified_time),
        }
    }
}

/// The key one namespace is stored under: its instance, then its name.
///
/// A `BTreeMap` over this pair orders every namespace of one instance together and in
/// name order, so a listing pages in one stable order and a page token can name the key
/// to resume at.
pub fn namespace_key(instance_id: &str, name: &str) -> (String, String) {
    (instance_id.to_string(), name.to_string())
}

/// How many data-integration events one instance holds in this store.
///
/// The same reasoning as [`MAX_DATASETS_PER_INSTANCE`]: the store keeps every event for
/// the life of the process, so the entry COUNT per instance is the growth term a caller
/// controls (each record's size is already bounded by the modelled `@length` constraints
/// on its members, and the event's payload is not stored at all). The bound belongs at
/// the one site that ADDS an event — `SendDataIntegrationEvent`, which declares the
/// modelled `ServiceQuotaExceededException` (HTTP 402) that a send past the cap is refused
/// with, and which no read declares — so a growth site checks
/// [`ComAmazonawsSupplychainGalaxypublicapigatewayState::data_integration_events_at_capacity`]
/// before inserting. Together with the reclamation on instance deletion
/// ([`ComAmazonawsSupplychainGalaxypublicapigatewayState::drop_instance_events`]) this is
/// a bound counted per LIVE instance, so instances coming and going cannot grow the store
/// past [`MAX_INSTANCES`] times this number. The number is the emulator's own bound, not a
/// published AWS value: AWS quotas data-integration throughput as an adjustable limit, so
/// no single number is the contract, and this one is far above any ordinary workflow.
pub const MAX_DATA_INTEGRATION_EVENTS_PER_INSTANCE: usize = 500;

/// The key one data-integration event is stored under: its instance, then its `eventId`
/// — the two members `GetDataIntegrationEvent`'s URI carries as `@httpLabel`s, in that
/// order.
///
/// A `BTreeMap` over this pair keeps one instance's events contiguous and in `eventId`
/// order, so `ListDataIntegrationEvents` pages through one stable order and a page token
/// can name the key to resume at.
pub type DataIntegrationEventKey = (String, String);

/// The [`DataIntegrationEventKey`] for one event, so every handler builds the key the
/// same way.
pub fn data_integration_event_key(instance_id: &str, event_id: &str) -> DataIntegrationEventKey {
    (instance_id.to_string(), event_id.to_string())
}

/// One persisted data-integration event — the whole record, not the wire shape.
/// [`DataIntegrationEventRecord::to_wire`] projects it onto the modelled
/// `DataIntegrationEvent` structure, so every handler answers from exactly these stored
/// values.
///
/// The event's `data` payload is deliberately NOT part of the record: the modelled
/// `DataIntegrationEvent` shape carries no member for it (the payload is the sensitive
/// input of the send, and the reads report only the event's metadata and the load
/// execution it drove), so nothing a read answers could come from it.
#[derive(Debug, Clone)]
pub struct DataIntegrationEventRecord {
    pub instance_id: String,
    pub event_id: String,
    pub event_type: String,
    pub event_group_id: String,
    pub event_timestamp: SystemTime,
    /// Set only for the event type that targets a data-lake dataset directly; the model
    /// makes the member optional for exactly that reason.
    pub dataset_target_details: Option<DataIntegrationEventDatasetTargetDetails>,
}

impl DataIntegrationEventRecord {
    /// This record as the modelled `DataIntegrationEvent` shape the event reads return.
    pub fn to_wire(&self) -> DataIntegrationEvent {
        DataIntegrationEvent {
            instance_id: self.instance_id.clone(),
            event_id: self.event_id.clone(),
            event_type: self.event_type.clone(),
            event_group_id: self.event_group_id.clone(),
            event_timestamp: as_date_time(self.event_timestamp),
            dataset_target_details: self.dataset_target_details.clone(),
        }
    }
}

/// How a data-integration flow is addressed: `(instanceId, name)` — the two `@httpLabel`
/// members every flow operation's URI carries, in that order.
///
/// A `BTreeMap` keyed by it therefore groups one instance's flows contiguously and in
/// name order, the order `ListDataIntegrationFlows` pages through.
pub type FlowKey = (String, String);

/// The [`FlowKey`] for one flow, so every handler builds the key the same way.
pub fn flow_key(instance_id: &str, name: &str) -> FlowKey {
    (instance_id.to_string(), name.to_string())
}

/// The `S3` value of both `DataIntegrationFlowSourceType` and
/// `DataIntegrationFlowTargetType` (see
/// [`crate::ops::types::DATA_INTEGRATION_FLOW_SOURCE_TYPE_VALUES`]): the end of a flow
/// that reads or writes S3 objects, and so the one that is configured by `s3Source` /
/// `s3Target`.
pub const FLOW_ENDPOINT_TYPE_S3: &str = "S3";

/// The `DATASET` value of both of those enums: the end of a flow that reads or writes a
/// data-lake dataset, configured by `datasetSource` / `datasetTarget`.
pub const FLOW_ENDPOINT_TYPE_DATASET: &str = "DATASET";

/// The `SQL` value of `DataIntegrationFlowTransformationType` (see
/// [`crate::ops::types::DATA_INTEGRATION_FLOW_TRANSFORMATION_TYPE_VALUES`]): the
/// transformation that is configured by `sqlTransformation`. Its sibling `NONE` carries
/// no configuration at all.
pub const FLOW_TRANSFORMATION_TYPE_SQL: &str = "SQL";

/// How a flow write refuses a request that declares a source, target or transformation
/// type but carries none of the configuration that type is configured by.
///
/// SOURCE: the configuration members themselves — each `…Type` member is the
/// discriminator for the sibling that configures it (`s3Source` is "The S3
/// DataIntegrationFlow source", `datasetTarget` "The dataset DataIntegrationFlow
/// target", `sqlTransformation` "The SQL DataIntegrationFlow transformation
/// configuration"), so a declared type with its configuration absent describes no source,
/// target or transformation at all and cannot be stored as one. The model cannot express
/// the dependency, so it is refused here with the modelled 400. No recording fixes AWS's
/// own wording for this refusal, so a later reader re-verifies the sentence against one.
pub fn missing_flow_configuration_message(member: &str, discriminator: &str, value: &str) -> String {
    format!("{member} is required when {discriminator} is {value}")
}

/// The first declared flow type whose configuration the request omitted, as the sentence
/// `ValidationException` reports it — `None` when every declared type carries its
/// configuration.
///
/// Shared by the flow writes so the create and the update refuse the same request the
/// same way. A type this build does not know (the enums are open on the wire) constrains
/// nothing, so it is left to the modelled enum constraint rather than guessed at here.
pub fn flow_configuration_problem(
    sources: &[DataIntegrationFlowSource],
    transformation: &DataIntegrationFlowTransformation,
    target: &DataIntegrationFlowTarget,
) -> Option<String> {
    for source in sources {
        let missing = match source.source_type.as_str() {
            FLOW_ENDPOINT_TYPE_S3 => source.s3_source.is_none().then_some("s3Source"),
            FLOW_ENDPOINT_TYPE_DATASET => {
                source.dataset_source.is_none().then_some("datasetSource")
            }
            _ => None,
        };
        if let Some(member) = missing {
            return Some(missing_flow_configuration_message(
                member,
                "sourceType",
                &source.source_type,
            ));
        }
    }

    let missing_target = match target.target_type.as_str() {
        FLOW_ENDPOINT_TYPE_S3 => target.s3_target.is_none().then_some("s3Target"),
        FLOW_ENDPOINT_TYPE_DATASET => target.dataset_target.is_none().then_some("datasetTarget"),
        _ => None,
    };
    if let Some(member) = missing_target {
        return Some(missing_flow_configuration_message(
            member,
            "targetType",
            &target.target_type,
        ));
    }

    if transformation.transformation_type == FLOW_TRANSFORMATION_TYPE_SQL
        && transformation.sql_transformation.is_none()
    {
        return Some(missing_flow_configuration_message(
            "sqlTransformation",
            "transformationType",
            &transformation.transformation_type,
        ));
    }

    None
}

/// How a flow read refuses a name this instance holds no flow under
/// (`ResourceNotFoundException`, HTTP 404).
///
/// No recording fixes this sentence, so its FORM is this service's recorded not-found
/// family — one fixed sentence naming the resource and echoing nothing from the request,
/// as the recorded instance read ("Instance not found.") and the namespace read
/// ([`NAMESPACE_NOT_FOUND_MESSAGE`]) both answer; the flow CONFLICT sentence
/// ([`flow_conflict_message`]) is worded by its own class, which is why it is not the
/// model for this one. Stated once, here, so the flow operations cannot drift into
/// several sentences for one refusal, and re-verified against a recording when one
/// covers this read.
pub const FLOW_NOT_FOUND_MESSAGE: &str = "Flow not found.";

/// How a flow create refuses a `(instanceId, name)` pair that already holds one
/// (`ConflictException`, HTTP 409).
///
/// Its SOURCE is the recorded wire response of a second create for a name an instance
/// already holds: that call is answered `ConflictException` rather than overwriting the
/// flow, and the sentence is fixed except for the flow name, which comes from the
/// request's `name` — the service spells the resource "Flow", labels the name
/// "with flow name:", and ends with the inflected "already exists". The `instanceId`,
/// though part of the resource identity, is NOT part of the sentence. Stated once, here,
/// so the flow operations cannot drift into several sentences for one refusal.
pub fn flow_conflict_message(name: &str) -> String {
    format!("Flow with flow name: {name} already exists")
}

/// The number of data-integration flows one instance holds in this store.
///
/// Same reasoning as [`MAX_INSTANCES`]: the store keeps its records for the life of the
/// process, each record's size is already bounded by the modelled `@length` constraints
/// on its members, so the entry COUNT is the remaining growth term and it is capped at the
/// one site that adds a flow — refused with the modelled `ServiceQuotaExceededException`
/// (HTTP 402) that `CreateDataIntegrationFlow` declares and no read does. The number is
/// the emulator's own bound, not a published AWS value: AWS quotas flows per instance as
/// an adjustable limit, so no single number is the contract, and this one is far above any
/// ordinary workflow.
pub const MAX_FLOWS_PER_INSTANCE: usize = 100;

/// One persisted data-integration flow — the whole record, not the wire shape.
/// [`DataIntegrationFlowRecord::to_wire`] projects it onto the modelled
/// `DataIntegrationFlow` structure, so every handler answers from exactly these stored
/// values.
#[derive(Debug, Clone)]
pub struct DataIntegrationFlowRecord {
    pub instance_id: String,
    pub name: String,
    pub sources: Vec<DataIntegrationFlowSource>,
    pub transformation: DataIntegrationFlowTransformation,
    pub target: DataIntegrationFlowTarget,
    pub created_time: SystemTime,
    pub last_modified_time: SystemTime,
    /// The flow's tags. `tags` is `notProperty` on the create request and absent from the
    /// `DataIntegrationFlow` shape, so they are stored for the tagging operations to
    /// report rather than projected onto the wire shape here.
    pub tags: BTreeMap<String, String>,
}

impl DataIntegrationFlowRecord {
    /// This record as the modelled `DataIntegrationFlow` shape every flow read returns.
    pub fn to_wire(&self) -> DataIntegrationFlow {
        DataIntegrationFlow {
            instance_id: self.instance_id.clone(),
            name: self.name.clone(),
            sources: self.sources.clone(),
            transformation: self.transformation.clone(),
            target: self.target.clone(),
            created_time: as_date_time(self.created_time),
            last_modified_time: as_date_time(self.last_modified_time),
        }
    }
}

/// Per-service state for com-amazonaws-supplychain-galaxypublicapigateway. A handler reaches it through [`service_state`].
#[derive(Debug, Default)]
pub struct ComAmazonawsSupplychainGalaxypublicapigatewayState {
    /// Every live instance, keyed by `instanceId`. A `BTreeMap` so a lookup by id is a
    /// key lookup and a page token can name the record to resume at; the order
    /// `ListInstances` reports is the records' creation order reversed
    /// ([`InstanceRecord::creation_seq`]), which that handler builds.
    pub instances: BTreeMap<String, InstanceRecord>,
    /// The last value [`Self::next_instance_creation_seq`] handed out, so each created
    /// instance records where it falls in this store's creation order.
    pub instance_creation_seq: u64,
    /// `clientToken` -> `instanceId`, so a replayed `CreateInstance` (the operation is
    /// `@idempotent` and its `clientToken` is an `@idempotencyToken`) returns the
    /// instance the first call created instead of creating a second one.
    pub idempotency: HashMap<String, String>,
    /// Every data lake namespace a caller created, keyed by [`namespace_key`]
    /// (`instanceId` then `name`) — the pair that identifies one namespace, as the
    /// create's two `@httpLabel` members do.
    pub namespaces: BTreeMap<(String, String), DataLakeNamespaceRecord>,
    /// Every live data-lake dataset, keyed by [`DatasetKey`]. A `BTreeMap` so an
    /// instance's datasets are contiguous and in namespace-then-name order.
    pub datasets: BTreeMap<DatasetKey, DataLakeDatasetRecord>,
    /// Every data-integration event the service holds, keyed by
    /// [`DataIntegrationEventKey`] (`instanceId` then `eventId`) — the pair an event read
    /// addresses one by. A `BTreeMap` so one instance's events are contiguous and in
    /// `eventId` order, which is the order `ListDataIntegrationEvents` pages through.
    pub events: BTreeMap<DataIntegrationEventKey, DataIntegrationEventRecord>,
    /// Every live data-integration flow, keyed by [`FlowKey`] (`instanceId` then `name`)
    /// — the pair that identifies one flow, as every flow operation's two `@httpLabel`
    /// members do.
    pub flows: BTreeMap<FlowKey, DataIntegrationFlowRecord>,
}

impl ComAmazonawsSupplychainGalaxypublicapigatewayState {
    /// Materialise every state transition the clock already owes at `now`.
    ///
    /// Called at the top of each instance operation — including the reporting ones —
    /// because the `Initializing` -> `Active` transition was owed at `now` whether or
    /// not anybody called: it is the clock's work, not the reader's. Nothing else about
    /// a record changes here, so repeated reads stay identical once the transition is
    /// done.
    ///
    /// The scan runs under the state lock and touches every stored record, so its cost
    /// is bounded by [`MAX_INSTANCES`] — the cap `CreateInstance` enforces — and by
    /// nothing a caller controls beyond it.
    pub fn materialize_due_transitions(&mut self, now: SystemTime) {
        for record in self.instances.values_mut() {
            if record.state == INSTANCE_STATE_INITIALIZING && record.active_due <= now {
                record.state = INSTANCE_STATE_ACTIVE.to_string();
            }
        }
    }

    /// The creation-order position of the next instance this store takes: strictly
    /// increasing, so the instance that gets a larger value was created later. A counter
    /// rather than a wall-clock stamp because two creates can land on one instant, and
    /// `ListInstances` has to put them in a definite order anyway.
    pub fn next_instance_creation_seq(&mut self) -> u64 {
        self.instance_creation_seq = self.instance_creation_seq.saturating_add(1);
        self.instance_creation_seq
    }

    /// Whether a further `CreateInstance` would push either caller-driven store past its
    /// cap ([`MAX_INSTANCES`] / [`MAX_IDEMPOTENCY_TOKENS`]).
    ///
    /// Checked at that one growth site, before anything is inserted; a replay of a token
    /// already in the table inserts nothing and so is answered even at the cap.
    pub fn at_capacity(&self) -> bool {
        self.instances.len() >= MAX_INSTANCES || self.idempotency.len() >= MAX_IDEMPOTENCY_TOKENS
    }

    /// A fresh `instanceId`: a lowercase-hex v4 UUID, as the model's `UUID` shape
    /// requires (`^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`),
    /// that no live instance already holds.
    ///
    /// `None` when [`ID_MINT_ATTEMPTS`] draws collided — which cannot happen in
    /// practice, so the caller answers it with a modelled server error rather than
    /// spinning under the state lock.
    pub fn fresh_instance_id(&self) -> Option<String> {
        (0..ID_MINT_ATTEMPTS)
            .map(|_| new_uuid_v4())
            .find(|candidate| !self.instances.contains_key(candidate))
    }

    /// The stored namespace `name` of instance `instance_id`, or `None` when this
    /// instance's data lake holds no namespace under that name.
    pub fn namespace(&self, instance_id: &str, name: &str) -> Option<&DataLakeNamespaceRecord> {
        self.namespaces.get(&namespace_key(instance_id, name))
    }

    /// How many namespaces `instance_id`'s data lake holds — one range scan over the keys
    /// that instance's namespaces occupy, which is what
    /// [`MAX_NAMESPACES_PER_INSTANCE`] caps.
    pub fn namespace_count(&self, instance_id: &str) -> usize {
        self.namespaces
            .range(namespace_key(instance_id, "")..)
            .take_while(|((stored, _), _)| stored == instance_id)
            .count()
    }

    /// Whether a further namespace create for `instance_id` would push its data lake
    /// past [`MAX_NAMESPACES_PER_INSTANCE`]. Checked at that one growth site, before
    /// anything is inserted.
    pub fn namespaces_at_capacity(&self, instance_id: &str) -> bool {
        self.namespace_count(instance_id) >= MAX_NAMESPACES_PER_INSTANCE
    }

    /// How many datasets `instance_id` holds, across all of its namespaces — what
    /// [`MAX_DATASETS_PER_INSTANCE`] caps.
    pub fn dataset_count(&self, instance_id: &str) -> usize {
        self.datasets
            .keys()
            .filter(|(instance, _, _)| instance == instance_id)
            .count()
    }

    /// Drop every dataset of `instance_id`: a dataset is addressed by its instance, so
    /// none of them outlives the instance whose deletion removed it.
    pub fn drop_instance_datasets(&mut self, instance_id: &str) {
        self.datasets
            .retain(|(instance, _, _), _| instance != instance_id);
    }

    /// How many data-integration events `instance_id` holds — one range scan over the
    /// contiguous span its events occupy.
    pub fn data_integration_event_count(&self, instance_id: &str) -> usize {
        self.instance_events(instance_id).count()
    }

    /// Every stored event of `instance_id`, in `eventId` order: the contiguous span of
    /// the key space that instance's events occupy.
    pub fn instance_events<'a>(
        &'a self,
        instance_id: &'a str,
    ) -> impl Iterator<Item = &'a DataIntegrationEventRecord> + 'a {
        self.events
            .range(data_integration_event_key(instance_id, "")..)
            .take_while(move |((instance, _), _)| instance == instance_id)
            .map(|(_, record)| record)
    }

    /// Whether a further event for `instance_id` would push its store past
    /// [`MAX_DATA_INTEGRATION_EVENTS_PER_INSTANCE`]. For the one site that adds an event
    /// to check before inserting, as the other growth sites of this store do.
    pub fn data_integration_events_at_capacity(&self, instance_id: &str) -> bool {
        self.data_integration_event_count(instance_id) >= MAX_DATA_INTEGRATION_EVENTS_PER_INSTANCE
    }

    /// Drop every data-integration event of `instance_id`: an event is addressed by its
    /// instance (and by nothing else), so none of them outlives the instance whose
    /// deletion removed it — the same rule, and the same reason, as
    /// [`Self::drop_instance_datasets`] and [`Self::drop_instance_namespaces`].
    pub fn drop_instance_events(&mut self, instance_id: &str) {
        self.events
            .retain(|(instance, _), _| instance != instance_id);
    }

    /// Drop every namespace of `instance_id`, for the same reason as
    /// [`Self::drop_instance_datasets`]: a namespace is addressed by its instance, so it
    /// does not outlive the instance whose deletion removed it — which is also what keeps
    /// [`MAX_NAMESPACES_PER_INSTANCE`], a bound counted per live instance, dominating the
    /// store's growth across instances coming and going.
    pub fn drop_instance_namespaces(&mut self, instance_id: &str) {
        self.namespaces
            .retain(|(instance, _), _| instance != instance_id);
    }

    /// The stored flow `name` of instance `instance_id`, or `None` when that instance
    /// holds no flow under that name.
    pub fn flow(&self, instance_id: &str, name: &str) -> Option<&DataIntegrationFlowRecord> {
        self.flows.get(&flow_key(instance_id, name))
    }

    /// How many flows `instance_id` holds — one range scan over the keys that instance's
    /// flows occupy, which is what [`MAX_FLOWS_PER_INSTANCE`] caps.
    pub fn flow_count(&self, instance_id: &str) -> usize {
        self.flows
            .range(flow_key(instance_id, "")..)
            .take_while(|((stored, _), _)| stored == instance_id)
            .count()
    }

    /// Whether a further flow create for `instance_id` would push it past
    /// [`MAX_FLOWS_PER_INSTANCE`]. Checked at that one growth site, before anything is
    /// inserted.
    pub fn flows_at_capacity(&self, instance_id: &str) -> bool {
        self.flow_count(instance_id) >= MAX_FLOWS_PER_INSTANCE
    }

    /// Drop every flow of `instance_id`, for the same reason as
    /// [`Self::drop_instance_datasets`]: a flow is addressed by its instance, so it does
    /// not outlive the instance whose deletion removed it.
    pub fn drop_instance_flows(&mut self, instance_id: &str) {
        self.flows.retain(|(instance, _), _| instance != instance_id);
    }
}

/// A fresh random (version 4) UUID in its canonical lowercase hyphenated form.
///
/// Drawn through `emulator_telemetry`'s own v4 generator, a crate this service already
/// depends on, so minting an id adds no dependency — the same seam
/// `services/service-com-amazonaws-signin-signin/src/state.rs` draws its opaque values
/// through.
fn new_uuid_v4() -> String {
    ::emulator_telemetry::SessionId::random().as_str().to_string()
}

/// Whether `value` is an `instanceId` of the model's `UUID` shape: length 36 and
/// `^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`.
///
/// Stated once, here, so the handler that validates a resume key and the tests that
/// pin the shape of a minted id cannot drift apart. `Uuid::parse_str` is deliberately
/// NOT the check: it also accepts the braced, urn and simple forms and uppercase hex,
/// none of which the model's pattern allows.
pub fn is_modelled_uuid(value: &str) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let groups: Vec<&str> = value.split('-').collect();
    value.len() == 36
        && groups.len() == GROUPS.len()
        && groups.iter().zip(GROUPS).all(|(group, width)| {
            group.len() == width
                && group
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

/// This service's store in the shared engine: call it with a handler's `&*self.engine`.
///
/// Answer `None` with a modelled server error, never with a fresh engine.
pub fn service_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<ComAmazonawsSupplychainGalaxypublicapigatewayState>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<ComAmazonawsSupplychainGalaxypublicapigatewayState>)
}
