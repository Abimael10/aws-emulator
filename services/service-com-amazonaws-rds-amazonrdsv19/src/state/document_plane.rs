// SPDX-License-Identifier: CC0-1.0

//! The DOCUMENT data plane of a DB instance whose engine is Amazon DocumentDB's: the socket a
//! client connects to and speaks the engine's own wire on.
//!
//! Amazon DocumentDB, Amazon Neptune and Amazon RDS are three views of ONE AWS front door
//! (`src/service.rs`): an AWS SDK DocumentDB client signs `rds`, so a `CreateDBCluster` /
//! `CreateDBInstance` with `Engine: docdb` is served HERE, and the resource it creates is an
//! Amazon DocumentDB cluster. Its `Endpoint{Address,Port}` is "the connection endpoint" a
//! client dials — so, exactly as for the engines whose SQL wires this service serves
//! ([`super::data_plane`]), such an instance is given a socket of the EMULATOR's own and
//! publishes the address that socket answers on: a name under Amazon DocumentDB's own DNS zone
//! resolves to nothing on the machine this emulator runs on and would be no connection
//! endpoint at all.
//!
//! What answers there is the ONE implementation of that wire this tree has, which the Amazon
//! DocumentDB control plane's own crate owns
//! (`service_com_amazonaws_docdb_amazonrdsv19::state::cluster_data_plane`), over the
//! documents that crate holds for the CLUSTER — keyed by the cluster's immutable
//! `DbClusterResourceId`, so a cluster created through either view of the door addresses one
//! set of documents. This module is only the Amazon RDS side of it: which engine is served
//! that way, where the facts a session is judged by are read from (THIS service's records),
//! and the handle the store holds so the port lives exactly as long as the instance.

use std::sync::Arc;

use ::service_com_amazonaws_docdb_amazonrdsv19::state::cluster_data_plane as document_wire;

use super::{ComAmazonawsRdsAmazonrdsv19State, DbInstanceRecord, RegionalKey, ServedEndpoint};

/// One instance's document-wire socket, as the store holds it. Dropping it closes the port and
/// severs every session on it.
pub type DocumentPlane = document_wire::InstanceDataPlane;

/// The address a document-wire socket answers on, as THIS service publishes one: the host the
/// core bound the listener to and the port it assigned, read off the plane the DocumentDB
/// crate opened.
#[must_use]
pub fn published_address(plane: &DocumentPlane) -> ServedEndpoint {
    let endpoint = plane.endpoint();
    ServedEndpoint {
        host: endpoint.host.clone(),
        port: endpoint.port,
    }
}

/// Whether the data plane of `engine` is the document wire this module serves.
///
/// `CreateDBClusterMessage$Engine` accepts `docdb` for an Amazon DocumentDB cluster, whose
/// clients speak the document wire. Every other engine this door accepts is served by its own
/// wire or by none ([`super::engine_wire`], [`super::api_plane_address`]).
#[must_use]
pub fn engine_serves_the_document_wire(engine: &str) -> bool {
    document_wire::engine_speaks_the_document_wire(engine)
}

/// Whether the endpoints of the cluster `named` are ENCRYPTED.
///
/// The engine carries this as a DB CLUSTER parameter, so the answer is the effective value of
/// that parameter for this cluster: what the cluster's own group sets, and the parameter's
/// documented default (`enabled`) where the group sets nothing — which is also the answer for
/// a cluster carrying a default group AWS provides, since such a group sets no parameter of
/// its own. The same reading the DocumentDB view of this door takes.
#[must_use]
fn endpoints_are_encrypted(
    store: &ComAmazonawsRdsAmazonrdsv19State,
    region: &str,
    named: &str,
) -> bool {
    let setting = store
        .resolve_db_cluster(region, named)
        .and_then(|cluster| cluster.db_cluster_parameter_group_name.as_ref())
        .and_then(|name| {
            store
                .db_cluster_parameter_groups
                .get(&(region.to_string(), name.to_ascii_lowercase()))
        })
        .and_then(|group| group.parameters.get(document_wire::TLS_PARAMETER))
        .map(|setting| setting.value.clone());
    !setting.is_some_and(|value| value.eq_ignore_ascii_case(document_wire::TLS_DISABLED))
}

/// The facts a session on the instance `key` names is judged against, read from THIS service's
/// records on every command — so a change the control plane makes while a session is open (a
/// failover moving the writer role, the instance deleted) is in force on its next command.
fn instance_facts(
    state: &::engine::ServiceState<ComAmazonawsRdsAmazonrdsv19State>,
    documents: Option<&::engine::ServiceState<document_wire::ClusterDocuments>>,
    key: &RegionalKey,
) -> document_wire::commands::EndpointFacts {
    let store = state.lock();
    let Some(instance) = store.db_instances.get(key) else {
        return document_wire::commands::EndpointFacts {
            exists: false,
            serving: false,
            status: super::DB_INSTANCE_STATUS_DELETING.to_string(),
            is_writer: false,
            engine_version: String::new(),
            credentials: None,
        };
    };
    let cluster = instance
        .db_cluster_identifier
        .as_deref()
        .and_then(|named| store.resolve_db_cluster(&key.0, named));
    document_wire::commands::EndpointFacts {
        exists: true,
        // An instance still being created has no engine to answer out of yet; one that is
        // available serves.
        serving: instance.status == super::DB_INSTANCE_STATUS_AVAILABLE,
        status: instance.status.clone(),
        // Writes go through the member the cluster writes through — the first of its members,
        // which is the one `IsClusterWriter` reports and which `FailoverDBCluster` moves — so
        // every other member serves reads only.
        is_writer: cluster.is_some_and(|cluster| {
            store
                .cluster_members(cluster)
                .first()
                .is_some_and(|writer| writer.identifier == instance.identifier)
        }),
        engine_version: cluster
            .and_then(|cluster| cluster.engine_version.clone())
            .or_else(|| instance.engine_version.clone())
            .unwrap_or_default(),
        // The credential a client authenticates against is the CLUSTER's, held with the
        // cluster's documents under its immutable resource id, so both views of this door
        // check a login against the one credential.
        credentials: cluster.and_then(|cluster| {
            documents?.lock().credentials(&cluster.resource_id)
        }),
    }
}

/// Open the socket the instance `record` is served on, for an instance of an engine whose data
/// plane is the document wire, and report it.
///
/// `None` for every other engine, for an instance that names no cluster — a DocumentDB
/// instance is always a member of one — and for an engine that vends no listeners, in which
/// case the instance keeps the DNS-form endpoint this service has always reported.
///
/// Called with NO store guard held: opening a socket is asynchronous, and this service holds
/// its store's lock across no await. The caller publishes
/// [`DocumentPlane::endpoint`] as the instance's `Endpoint` and keeps the handle on the store,
/// which is what holds the port open.
pub async fn open_document_plane<Eng: Send + Sync + 'static>(
    engine: &Eng,
    state: &::engine::ServiceState<ComAmazonawsRdsAmazonrdsv19State>,
    record: &DbInstanceRecord,
) -> Option<DocumentPlane> {
    if !engine_serves_the_document_wire(&record.engine) {
        return None;
    }
    let documents = document_wire::ClusterDocuments::of(engine);
    let named = record.db_cluster_identifier.clone()?;
    let (volume, encrypted) = {
        let store = state.lock();
        let cluster = store.resolve_db_cluster(&record.region, &named)?;
        let resource_id = cluster.resource_id.clone();
        let encrypted = endpoints_are_encrypted(&store, &record.region, &named);
        // The cluster's storage is taken under this service's own store, the lock order the
        // shared register fixes.
        (documents.as_ref()?.lock().volume(&resource_id), encrypted)
    };
    let key: RegionalKey = (record.region.clone(), record.identifier.clone());
    let facts: document_wire::FactsSource = {
        let state = state.clone();
        let documents = documents.clone();
        Arc::new(move || instance_facts(&state, documents.as_ref(), &key))
    };
    document_wire::open_instance_data_plane(engine, facts, volume, encrypted).await
}

/// Record the credential a client of the cluster `resource_id`'s endpoints authenticates
/// against, derived from the master user and password its create or modify carried.
///
/// What is kept is the SCRAM credential a login is checked against, never the password or any
/// form it can be recovered from, and it is held with the cluster's documents rather than on
/// this service's record, because both belong to the CLUSTER rather than to the view of this
/// front door that created it. Does nothing for a create that named no master user, whose
/// endpoints then have no credential to check a client against, and nothing on an engine that
/// vends no such store.
pub fn record_master_credential<Eng: Send + Sync + 'static>(
    engine: &Eng,
    resource_id: &str,
    username: Option<&str>,
    password: Option<&str>,
) {
    let (Some(username), Some(password)) = (username, password) else {
        return;
    };
    if let Some(documents) = document_wire::ClusterDocuments::of(engine) {
        documents.lock().set_credentials(
            resource_id,
            Some(document_wire::auth::ScramCredentials::derive(
                username, password,
            )),
        );
    }
}

/// Drop everything the cluster `resource_id` held in the shared document storage, which the
/// delete of the cluster does: the documents and the credential are the cluster's, so nothing
/// of them outlives it.
pub fn forget_cluster_documents<Eng: Send + Sync + 'static>(engine: &Eng, resource_id: &str) {
    if let Some(documents) = document_wire::ClusterDocuments::of(engine) {
        documents.lock().forget(resource_id);
    }
}
