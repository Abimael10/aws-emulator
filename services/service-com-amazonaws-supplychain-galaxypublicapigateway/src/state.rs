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
    DataIntegrationFlowExecution, DataIntegrationFlowExecutionOutputMetadata,
    DataIntegrationFlowExecutionSourceInfo, DataIntegrationFlowSource, DataIntegrationFlowTarget,
    DataIntegrationFlowTransformation, DataLakeDataset, DataLakeDatasetPartitionSpec,
    DataLakeDatasetSchema, DataLakeNamespace, Instance,
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

/// How this service refuses a call whose `instanceId` names no instance it holds
/// (`ResourceNotFoundException`, HTTP 404).
///
/// Its SOURCE is the recorded wire response of such a call: the sentence is FIXED — it
/// names the resource kind and nothing else, and in particular does not echo the
/// `instanceId` the request asked about, as the flow read ([`FLOW_NOT_FOUND_MESSAGE`])
/// answers too. (The namespace and dataset refusals are recorded NAMING their resource —
/// see [`namespace_not_found_message`] — so this family is read from each recording
/// rather than assumed uniform.) Stated once, here, so no instance-scoped handler can
/// drift into a sentence of its own for this one refusal.
pub const INSTANCE_NOT_FOUND_MESSAGE: &str = "Instance not found.";

/// How this service refuses a call that asks to MUTATE an instance which is not in
/// `Active` ([`INSTANCE_STATE_ACTIVE`]) — a `ValidationException` (HTTP 400), because what
/// refused the call is the instance's state machine rather than the instance's existence.
///
/// Its SOURCE is the recorded wire response of an `UpdateInstance` issued while the
/// addressed instance was still settling its deletion. The whole sentence is fixed: it
/// names the state the operation requires and carries nothing from the request. Stated
/// once, here, so every operation gated on an instance's state states one sentence.
pub const INSTANCE_NOT_ACTIVE_MESSAGE: &str =
    "Instance must be in Active state to perform this operation.";

/// How long after `CreateInstance` the clock owes an instance its
/// `Initializing` -> `Active` transition.
///
/// CHOSEN, not documented: AWS states only that `CreateInstance` is asynchronous and
/// returns `Initializing`, never how long initialization takes. The span is deliberately
/// non-zero so a caller that reads the instance back immediately still observes
/// `Initializing` (and so a readiness poll has something to converge on), and short
/// enough that the poll terminates promptly.
pub const INSTANCE_ACTIVATION_DELAY: Duration = Duration::from_millis(500);

/// How long after `DeleteInstance` the clock owes an instance its removal from the store.
///
/// CHOSEN, not documented: AWS answers `DeleteInstance` with the instance in `Deleting`
/// and never states how long the deletion then takes. The span is deliberately non-zero,
/// because `Deleting` is a state the recorded service lets a caller OBSERVE: an instance
/// whose deletion has been accepted is still addressable while it settles — a read reports
/// it in `Deleting` and a mutation is refused by the state gate
/// ([`INSTANCE_NOT_ACTIVE_MESSAGE`]) rather than as a not-found — and only once the span
/// has passed does the record, and everything addressed by it, cease to exist.
pub const INSTANCE_DELETION_DELAY: Duration = Duration::from_secs(3);

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

/// The DNS names an instance ANNOUNCES, and the WebApp domain label a create that named
/// none is answered with: the one place this service decides which host it hands a client,
/// so no handler composes an address of its own.
mod instance_domains;
pub use instance_domains::*;

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
    /// The instant at which this instance's accepted deletion settles — when the record,
    /// and everything addressed by it, leaves the store. `None` for an instance no
    /// `DeleteInstance` has been accepted for. Stored rather than slept on, for the same
    /// reason as [`Self::active_due`]: the removal is owed at that instant whether or not
    /// anybody calls, so whichever call next observes the store materialises it.
    pub deletion_due: Option<SystemTime>,
}

impl InstanceRecord {
    /// A brand-new instance in `Initializing`, owned by the emulator's account.
    ///
    /// The optional members (`instanceName`, `instanceDescription`, `kmsKeyArn`,
    /// `webAppDnsDomain`, `tags`) are left absent for the caller to fill from the
    /// request, so a member the request omitted stays absent rather than being
    /// invented — except `webAppDnsDomain`, which every instance has: the create either
    /// echoes the one the request named or mints one
    /// ([`ComAmazonawsSupplychainGalaxypublicapigatewayState::fresh_web_app_dns_domain`]).
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
            deletion_due: None,
        }
    }

    /// This record as the `Instance` shape every instance operation returns, with its web
    /// app announced at `endpoint` — the `host[:port]` the request being answered reached
    /// this emulator at ([`announced_instance_endpoint`]).
    ///
    /// `primaryDomain` and `idcPortalDomain` are where a client reaches the instance's web
    /// app and its IAM Identity Center portal: this emulator serves both at the one address
    /// it answers on, so both members carry that address — the endpoint the client is
    /// already talking to — and neither names a host nothing here serves. The instance's own
    /// identity stays in `webAppDnsDomain` and `instanceId`, the members this API carries it
    /// in; the recorded responses assert the two host members shape-only, pinning no second
    /// host.
    ///
    /// The address is passed in rather than read here, because it is a property of the
    /// REQUEST being answered while this record is the stored instance: an instance reached
    /// through one addressing is never answered another addressing's host, and nothing about
    /// the announced hosts is persisted.
    pub fn to_wire(&self, endpoint: &str) -> Instance {
        let announced = endpoint.to_string();
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
            primary_domain: Some(announced.clone()),
            idc_portal_domain: Some(announced),
        }
    }

    /// Record a change made at `now`: a mutation moves `lastModifiedTime` and nothing
    /// else. `versionNumber` deliberately does NOT move — it is the instance's own
    /// version ([`INSTANCE_VERSION_NUMBER`]), not a count of the writes it has taken.
    pub fn touch(&mut self, now: SystemTime) {
        self.last_modified_time = now;
    }

    /// Whether this instance is in the one state a mutation of it is served in
    /// ([`INSTANCE_STATE_ACTIVE`]). An instance still initializing, or already settling
    /// its deletion, is not.
    pub fn is_active(&self) -> bool {
        self.state == INSTANCE_STATE_ACTIVE
    }

    /// Accept a deletion of this instance at `now`: the record moves to `Deleting` and
    /// the clock owes its removal [`INSTANCE_DELETION_DELAY`] later. The record STAYS in
    /// the store until then, which is what makes `Deleting` an observable state.
    ///
    /// Idempotent, as `DeleteInstance` is: a second accepted deletion of an instance
    /// already settling leaves the first one's due instant, so repeating the call does not
    /// push the removal further out.
    pub fn begin_deletion(&mut self, now: SystemTime) {
        if self.deletion_due.is_some() {
            return;
        }
        self.state = INSTANCE_STATE_DELETING.to_string();
        self.deletion_due = Some(now + INSTANCE_DELETION_DELAY);
        self.touch(now);
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

/// How a namespace operation refuses a name this instance's data lake does not hold
/// (`ResourceNotFoundException`, HTTP 404).
///
/// Its SOURCE is the recorded wire response of a namespace read and of a namespace delete
/// addressed to a name the data lake does not hold: the FIXED text is "Namespace " and
/// " not exist" — the service capitalises the resource noun and writes the uninflected
/// "not exist", exactly as the sibling dataset refusal ([`dataset_not_found_message`])
/// does — and the only interpolated part is the namespace name, the `name` `@httpLabel`
/// of the namespace URI. The `instanceId`, though part of the resource identity, is NOT
/// part of the sentence. Stated once, here, so the namespace operations cannot drift into
/// several sentences for one refusal.
pub fn namespace_not_found_message(name: &str) -> String {
    format!("Namespace {name} not exist")
}

/// How a namespace delete refuses a namespace that still holds at least one dataset
/// (`ValidationException`, HTTP 400).
///
/// Its SOURCE is the recorded wire response of such a delete: the namespace and its
/// datasets are left untouched and the caller is told both what blocks the delete and
/// what to do about it. The FIXED text is "Datasets found under the namespace " and
/// ". Please delete them before deleting the namespace"; the only interpolated part is the
/// namespace name from the request.
pub fn namespace_not_empty_message(name: &str) -> String {
    format!(
        "Datasets found under the namespace {name}. Please delete them before deleting the \
         namespace"
    )
}

/// How a paginated read of this service refuses a `nextToken` it did not issue.
///
/// A continuation token carries the position it names INSIDE it, so a value a caller built
/// (or one another listing issued) names a position this listing never offered. The APIs
/// document a refusal for such a value, never a page, and the modelled
/// `ValidationException` (HTTP 400) is the channel every paginated operation here has for
/// it. Stated once, here, so the listings cannot drift into several sentences for one
/// refusal. No recording pins this sentence, so it is the emulator's own wording and
/// carries nothing from the request.
pub const UNISSUED_PAGE_TOKEN_MESSAGE: &str = "nextToken is not a pagination token this service issued";

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

/// The sentence a delete addressed to a namespace the instance comes with is refused
/// with (`ValidationException`, HTTP 400) — one of [`PREDEFINED_NAMESPACES`], which is
/// what [`is_predefined_namespace`] decides, so the refusal and the rule cannot drift
/// apart.
///
/// SOURCE: the `name` member of `DeleteDataLakeNamespaceRequest` — "Noted you cannot
/// delete pre-defined namespace like asc, default which are only deleted through instance
/// deletion" — which is why such a delete is a bad request rather than a not-found: the
/// namespace exists, it is just not a caller's to remove. The namespace the request
/// addressed is the only interpolated part, as it is in this service's other namespace
/// refusals. No recording fixes AWS's own wording for this one, so a later reader
/// re-verifies it against one.
pub fn predefined_namespace_delete_message(name: &str) -> String {
    format!("Cannot delete pre-defined namespace {name}")
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

/// How a data-integration event read refuses an `eventId` the addressed instance holds no
/// event under (`ResourceNotFoundException`, HTTP 404).
///
/// No recording fixes this sentence, so its FORM is this service's recorded not-found
/// family — one fixed sentence naming the resource and echoing nothing from the request, as
/// the instance read ([`INSTANCE_NOT_FOUND_MESSAGE`]) and the flow read
/// ([`FLOW_NOT_FOUND_MESSAGE`]) answer. Stated once, here, and re-verified against a
/// recording when one covers this read.
pub const EVENT_NOT_FOUND_MESSAGE: &str = "Event not found.";

/// The one `DataIntegrationEventType` whose event names its target dataset itself.
///
/// SOURCE: the `eventType` member's own documentation — "scn.data.dataset - Send data
/// directly to any specified dataset" — which is also the one type the `datasetTarget`
/// member is documented for ("The target dataset configuration for scn.data.dataset event
/// type"). Every other type names a dataset of the `asc` namespace the service keeps for
/// it.
pub const DATA_INTEGRATION_EVENT_TYPE_DATASET: &str = "scn.data.dataset";

/// The `DataIntegrationEventDatasetLoadStatus` an accepted event's load reports.
///
/// RECORDED behaviour: an event accepted for a dataset that exists reports its load as
/// succeeded — the value `SUCCEEDED` of that enum
/// (`smithy_query(query="shape", shape="DataIntegrationEventDatasetLoadStatus")`).
pub const DATASET_LOAD_STATUS_SUCCEEDED: &str = "SUCCEEDED";

/// How a send refuses a `datasetTarget` whose `datasetIdentifier` names no dataset this
/// instance's data lake holds (`ResourceNotFoundException`, HTTP 404).
///
/// Its SOURCE is the recorded wire response of such a send: the fixed text is "Target
/// dataset resource arn: " / " is not valid", and the only interpolated part is the ARN
/// the request's `datasetTarget.datasetIdentifier` carried. Stated once, here, beside the
/// service's other refusal sentences.
pub fn dataset_target_not_valid_message(dataset_identifier: &str) -> String {
    format!("Target dataset resource arn: {dataset_identifier} is not valid")
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
/// as the recorded instance read ("Instance not found.") answers; the flow CONFLICT
/// sentence
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

/// How a flow-execution read refuses an `executionId` the addressed flow does not hold
/// (`ResourceNotFoundException`, HTTP 404).
///
/// No recording fixes this sentence, so its FORM is this service's recorded not-found
/// family — one fixed sentence naming the resource and echoing nothing from the request,
/// as the instance read ([`INSTANCE_NOT_FOUND_MESSAGE`]) and the flow read
/// ([`FLOW_NOT_FOUND_MESSAGE`]) answer. Stated once, here, and re-verified against a
/// recording when one covers this read.
pub const FLOW_EXECUTION_NOT_FOUND_MESSAGE: &str = "Flow execution not found.";

/// The key one flow execution is stored under: its instance, its flow's name, then its
/// own `executionId` — the three `@httpLabel` members `GetDataIntegrationFlowExecution`'s
/// URI carries, in that order.
///
/// A `BTreeMap` over the triple keeps one flow's executions contiguous and in
/// `executionId` order, which is the order a listing of them pages through.
pub type FlowExecutionKey = (String, String, String);

/// The [`FlowExecutionKey`] for one execution, so every handler builds the key the same
/// way.
pub fn flow_execution_key(
    instance_id: &str,
    flow_name: &str,
    execution_id: &str,
) -> FlowExecutionKey {
    (
        instance_id.to_string(),
        flow_name.to_string(),
        execution_id.to_string(),
    )
}

/// One persisted run of a data-integration flow — the whole record, not the wire shape.
/// [`DataIntegrationFlowExecutionRecord::to_wire`] projects it onto the modelled
/// `DataIntegrationFlowExecution` structure, so a handler answers from exactly these
/// stored values.
///
/// A flow execution is created by the flow RUNNING, never by a caller: the model binds no
/// operation that starts one, so the only producers are this service's own ingestion
/// paths. Each member is held exactly as the modelled shape carries it, including the
/// ones a run leaves absent (an execution still in progress has no `endTime`, a
/// successful one no `message`).
#[derive(Debug, Clone)]
pub struct DataIntegrationFlowExecutionRecord {
    pub instance_id: String,
    pub flow_name: String,
    pub execution_id: String,
    pub status: Option<String>,
    pub source_info: Option<DataIntegrationFlowExecutionSourceInfo>,
    pub message: Option<String>,
    pub start_time: Option<SystemTime>,
    pub end_time: Option<SystemTime>,
    pub output_metadata: Option<DataIntegrationFlowExecutionOutputMetadata>,
}

impl DataIntegrationFlowExecutionRecord {
    /// This record as the modelled `DataIntegrationFlowExecution` shape the flow-execution
    /// read returns.
    pub fn to_wire(&self) -> DataIntegrationFlowExecution {
        DataIntegrationFlowExecution {
            instance_id: self.instance_id.clone(),
            flow_name: self.flow_name.clone(),
            execution_id: self.execution_id.clone(),
            status: self.status.clone(),
            source_info: self.source_info.clone(),
            message: self.message.clone(),
            start_time: self.start_time.map(as_date_time),
            end_time: self.end_time.map(as_date_time),
            output_metadata: self.output_metadata.clone(),
        }
    }
}

/// Which resource of this service one `AscResourceArn` names.
///
/// The ARN's resource portion is the inverse of the helpers that MINT it
/// ([`instance_arn`], [`namespace_arn`], [`dataset_arn`]), so a resource is addressed by
/// exactly the path its own ARN spells and nothing else resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaggedResource {
    /// One AWS Supply Chain instance, by its `instanceId`.
    Instance(String),
    /// One data lake namespace, by its [`namespace_key`].
    Namespace((String, String)),
    /// One data lake dataset, by its [`DatasetKey`].
    Dataset(DatasetKey),
}

/// The ARN of one AWS Supply Chain instance.
///
/// The modelled `AscResourceArn` pattern
/// (`^arn:aws:scn(?::([a-z0-9-]+):([0-9]+):instance)?/(<uuid>)[-_./A-Za-z0-9]*$`) is what
/// fixes the shape: the instance segment carries the `instanceId`, and a resource WITHIN
/// the instance extends this path ([`namespace_arn`], [`dataset_arn`]).
pub fn instance_arn(region: &str, instance_id: &str) -> String {
    ::engine::emulator_arn(ARN_SERVICE, region, &format!("instance/{instance_id}"))
}

/// Which resource `arn` names, or `None` for an ARN of this partition and account whose
/// resource portion names nothing this service mints.
/// Both forms the modelled pattern admits are read: the full
/// `arn:aws:scn:<region>:<account>:instance/<id>…` an operation of this service mints, and
/// the account-less `arn:aws:scn/<id>…` the pattern's optional group leaves out. The
/// REGION is not matched: this service's stores are keyed by the resource's own
/// identifiers rather than partitioned by region, so an ARN naming another region still
/// names the same resource. The ACCOUNT is matched, because an ARN naming another account
/// names no resource this emulator holds.
pub fn tagged_resource(arn: &str) -> Option<TaggedResource> {
    let rest = arn.strip_prefix(&format!("arn:aws:{ARN_SERVICE}"))?;
    let path = match rest.strip_prefix('/') {
        // `arn:aws:scn/<id>…`: no region, account or `instance` segment to read.
        Some(path) => path,
        None => {
            let mut fields = rest.strip_prefix(':')?.splitn(3, ':');
            let _region = fields.next()?;
            if fields.next()? != ::engine::emulator_account_id() {
                return None;
            }
            fields.next()?.strip_prefix("instance/")?
        }
    };
    let segments: Vec<&str> = path.split('/').collect();
    match segments.as_slice() {
        [instance_id] => Some(TaggedResource::Instance((*instance_id).to_string())),
        [instance_id, "namespaces", name] => {
            Some(TaggedResource::Namespace(namespace_key(instance_id, name)))
        }
        [instance_id, "namespaces", namespace, "datasets", name] => Some(TaggedResource::Dataset(
            dataset_key(instance_id, namespace, name),
        )),
        _ => None,
    }
}

/// How many tags one resource of this service carries in this store.
///
/// The number is the modelled bound, not the emulator's own: `TagMap`'s own `@length`
/// (`min: 0, max: 200`) is what the model admits in ONE request, so a resource cannot be
/// made to hold more than the model would let a caller send in a single call. Tag keys are
/// caller-supplied strings and a tag write MERGES into what the resource already carries,
/// so this is the growth term a caller controls and it is enforced at that one write site,
/// which refuses past the cap with the `ValidationException` (HTTP 400) `TagResource`
/// declares for an input the service cannot satisfy.
pub const MAX_TAGS_PER_RESOURCE: usize = 200;

/// How a tagging operation refuses a `resourceArn` that is well formed for this service
/// but names a resource the store does not hold (`ResourceNotFoundException`, HTTP 404).
///
/// The sentence is the one this service answers a read of THAT KIND of resource with, so a
/// tag call and a read of the same resource report one not-found rather than two: an
/// instance ARN gets [`INSTANCE_NOT_FOUND_MESSAGE`], a namespace ARN
/// [`namespace_not_found_message`], a dataset ARN [`dataset_not_found_message`]. An ARN
/// whose resource portion names no resource kind this service mints
/// ([`tagged_resource`] reads none) gets the same sentence stated for the resource it
/// failed to name, which keeps the family's form — a fixed sentence naming the resource and
/// echoing nothing from the request.
pub fn resource_not_found_message(arn: &str) -> String {
    match tagged_resource(arn) {
        Some(TaggedResource::Instance(_)) => INSTANCE_NOT_FOUND_MESSAGE.to_string(),
        Some(TaggedResource::Namespace((_, name))) => namespace_not_found_message(&name),
        Some(TaggedResource::Dataset((_, namespace, name))) => {
            dataset_not_found_message(&name, &namespace)
        }
        None => RESOURCE_NOT_FOUND_MESSAGE.to_string(),
    }
}

/// How this service refuses a call addressing a resource it holds no kind of record for:
/// the not-found sentence of the family ([`INSTANCE_NOT_FOUND_MESSAGE`] and the rest) with
/// the resource named only as what it is.
pub const RESOURCE_NOT_FOUND_MESSAGE: &str = "Resource not found.";

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
    /// `clientToken` -> the event it recorded, so a replayed `SendDataIntegrationEvent`
    /// (the operation is `@idempotent` and its `clientToken` is an `@idempotencyToken`)
    /// answers with the event the first call recorded instead of recording a second one.
    /// An entry is only ever written beside an event and dropped with it, so this table
    /// holds no more entries than there are stored events.
    pub event_idempotency: HashMap<String, DataIntegrationEventKey>,
    /// Every live data-integration flow, keyed by [`FlowKey`] (`instanceId` then `name`)
    /// — the pair that identifies one flow, as every flow operation's two `@httpLabel`
    /// members do.
    pub flows: BTreeMap<FlowKey, DataIntegrationFlowRecord>,
    /// Every run of a flow this service holds, keyed by [`FlowExecutionKey`]. A
    /// `BTreeMap` so one flow's executions are contiguous and in `executionId` order.
    pub flow_executions: BTreeMap<FlowExecutionKey, DataIntegrationFlowExecutionRecord>,
}

impl ComAmazonawsSupplychainGalaxypublicapigatewayState {
    /// Materialise every state transition the clock already owes at `now`.
    ///
    /// Called at the top of each instance operation — including the reporting ones —
    /// because both transitions were owed at `now` whether or not anybody called: they
    /// are the clock's work, not the reader's. Nothing else about a record changes here,
    /// so repeated reads stay identical once the transitions are done.
    ///
    /// Two transitions are owed:
    ///
    /// * `Initializing` -> `Active`, [`INSTANCE_ACTIVATION_DELAY`] after the create;
    /// * the REMOVAL of an instance whose deletion was accepted, together with everything
    ///   addressed by it ([`Self::remove_instance`]), [`INSTANCE_DELETION_DELAY`] after
    ///   the delete — which is why a `Deleting` record is still readable before then.
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
        let settled: Vec<String> = self
            .instances
            .values()
            .filter(|record| record.deletion_due.is_some_and(|due| due <= now))
            .map(|record| record.instance_id.clone())
            .collect();
        for instance_id in settled {
            self.remove_instance(&instance_id);
        }
    }

    /// Take `instance_id` out of this store, with everything addressed by it.
    ///
    /// Every other resource of this service is addressed by an `instanceId`, so none of
    /// them outlives the instance whose deletion removed it — which is also what keeps
    /// each per-instance bound counted over LIVE instances only. The idempotency tokens
    /// that named this instance go too, so a replayed `CreateInstance` cannot be answered
    /// with a record the store no longer holds.
    pub fn remove_instance(&mut self, instance_id: &str) {
        self.instances.remove(instance_id);
        self.idempotency.retain(|_, named| named != instance_id);
        self.drop_instance_datasets(instance_id);
        self.drop_instance_namespaces(instance_id);
        self.drop_instance_flows(instance_id);
        self.drop_instance_flow_executions(instance_id);
        self.drop_instance_events(instance_id);
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

    /// Every stored namespace of `instance_id`, in `name` order: the contiguous span of
    /// the key space that instance's namespaces occupy ([`namespace_key`] puts the
    /// instance first). That order is the one `ListDataLakeNamespaces` pages through, and
    /// it holds only namespaces a caller created — the data lake's pre-defined namespaces
    /// ([`PREDEFINED_NAMESPACES`]) are never stored, which is exactly what that listing
    /// documents.
    pub fn instance_namespaces<'a>(
        &'a self,
        instance_id: &'a str,
    ) -> impl Iterator<Item = &'a DataLakeNamespaceRecord> + 'a {
        self.namespaces
            .range(namespace_key(instance_id, "")..)
            .take_while(move |((instance, _), _)| instance == instance_id)
            .map(|(_, record)| record)
    }

    /// How many namespaces `instance_id`'s data lake holds — one range scan over the keys
    /// that instance's namespaces occupy, which is what
    /// [`MAX_NAMESPACES_PER_INSTANCE`] caps.
    pub fn namespace_count(&self, instance_id: &str) -> usize {
        self.instance_namespaces(instance_id).count()
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

    /// Every stored dataset of `instance_id`'s `namespace` whose name sorts at or after
    /// `from` (`""` for the whole namespace), in dataset-name order: the contiguous span of
    /// the key space those datasets occupy, which is the order `ListDataLakeDatasets` pages
    /// through. The span is SEEKED to `from` rather than scanned from the namespace's first
    /// dataset, so resuming a page costs the same whatever sits behind the resume key.
    pub fn namespace_datasets_from<'a>(
        &'a self,
        instance_id: &'a str,
        namespace: &'a str,
        from: &str,
    ) -> impl Iterator<Item = &'a DataLakeDatasetRecord> + 'a {
        self.datasets
            .range(dataset_key(instance_id, namespace, from)..)
            .take_while(move |((instance, stored_namespace, _), _)| {
                instance == instance_id && stored_namespace == namespace
            })
            .map(|(_, record)| record)
    }

    /// How many datasets `instance_id` holds under the namespace `namespace` — the length
    /// of the span [`Self::namespace_datasets_from`] walks, which is what a namespace
    /// delete has to find empty.
    pub fn namespace_dataset_count(&self, instance_id: &str, namespace: &str) -> usize {
        self.namespace_datasets_from(instance_id, namespace, "").count()
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
    /// [`Self::drop_instance_datasets`] and [`Self::drop_instance_namespaces`]. The
    /// idempotency entries naming those events go with them, so a token cannot outlive
    /// the event it names.
    pub fn drop_instance_events(&mut self, instance_id: &str) {
        self.events
            .retain(|(instance, _), _| instance != instance_id);
        // The idempotency tokens that named this instance's events go with them, so a
        // replayed send cannot be answered with an event the store no longer holds.
        self.event_idempotency
            .retain(|_, (instance, _)| instance != instance_id);
    }

    /// A fresh `eventId` for `instance_id`: a lowercase-hex v4 UUID, as the model's `UUID`
    /// shape requires, that this instance's event store does not already hold.
    ///
    /// `None` when [`ID_MINT_ATTEMPTS`] draws collided, for the same reason as
    /// [`Self::fresh_instance_id`].
    pub fn fresh_event_id(&self, instance_id: &str) -> Option<String> {
        (0..ID_MINT_ATTEMPTS).map(|_| new_uuid_v4()).find(|candidate| {
            !self
                .events
                .contains_key(&data_integration_event_key(instance_id, candidate))
        })
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

    /// The stored run `execution_id` of flow `flow_name` of instance `instance_id`, or
    /// `None` when that flow holds no execution under that id.
    pub fn flow_execution(
        &self,
        instance_id: &str,
        flow_name: &str,
        execution_id: &str,
    ) -> Option<&DataIntegrationFlowExecutionRecord> {
        self.flow_executions
            .get(&flow_execution_key(instance_id, flow_name, execution_id))
    }

    /// Drop every flow execution of `instance_id`, for the same reason as
    /// [`Self::drop_instance_flows`]: an execution belongs to a flow of that instance, so
    /// it does not outlive the instance whose deletion removed it.
    pub fn drop_instance_flow_executions(&mut self, instance_id: &str) {
        self.flow_executions
            .retain(|(instance, _, _), _| instance != instance_id);
    }

    /// The tags of the resource `arn` names, or `None` when this store holds no such
    /// resource.
    ///
    /// One resolution for both tagging directions ([`Self::resource_tags_mut`] is the
    /// write side), so what a tag read reports and what a tag write changes cannot
    /// disagree about which resource an ARN names.
    pub fn resource_tags(&self, arn: &str) -> Option<&BTreeMap<String, String>> {
        match tagged_resource(arn)? {
            TaggedResource::Instance(instance_id) => {
                self.instances.get(&instance_id).map(|record| &record.tags)
            }
            TaggedResource::Namespace(key) => self.namespaces.get(&key).map(|record| &record.tags),
            TaggedResource::Dataset(key) => self.datasets.get(&key).map(|record| &record.tags),
        }
    }

    /// The tags of the resource `arn` names, for a caller that is about to change them;
    /// `None` when this store holds no such resource.
    pub fn resource_tags_mut(&mut self, arn: &str) -> Option<&mut BTreeMap<String, String>> {
        match tagged_resource(arn)? {
            TaggedResource::Instance(instance_id) => self
                .instances
                .get_mut(&instance_id)
                .map(|record| &mut record.tags),
            TaggedResource::Namespace(key) => self
                .namespaces
                .get_mut(&key)
                .map(|record| &mut record.tags),
            TaggedResource::Dataset(key) => {
                self.datasets.get_mut(&key).map(|record| &mut record.tags)
            }
        }
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

/// Whether `name` has the shape the model gives `DataLakeNamespaceName`: length 1..50 and
/// `^[a-z0-9_]+$`.
///
/// Stated once, here, so the handler that validates a resume key and anything else that
/// has to recognise a namespace name cannot drift from the modelled constraint. The
/// modelled constraints already refuse a bad `name` MEMBER before a handler runs; this is
/// for a name that arrives inside an opaque value instead, where no member carries it.
pub fn is_modelled_namespace_name(name: &str) -> bool {
    const MAX_NAMESPACE_NAME_LENGTH: usize = 50;
    !name.is_empty()
        && name.len() <= MAX_NAMESPACE_NAME_LENGTH
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
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
