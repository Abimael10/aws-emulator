// SPDX-License-Identifier: CC0-1.0

//! How a signed request to the address a domain PUBLISHES is authorized, and what the
//! refusal looks like when it is not.
//!
//! Three rules, each over a SET of inputs rather than one recorded call:
//!
//! * **a request IAM refuses is answered the way AWS's own authorization layer answers one
//!   — `403` carrying a `Message` member whose sentence names the principal that signed,
//!   the `es:ESHttp*` action the request's METHOD asks for, and which of IAM's rules
//!   refused.** That layer sits in front of the cluster, so the answer is AWS's envelope
//!   and carries none of the engine's error document: the engine never saw the request.
//!   The rows span which rule refused (an explicit `Deny` in the domain's own access
//!   policy, an explicit `Deny` in the principal's identity policy, and nothing allowing
//!   at all) and the method the action is read from, so the action in the sentence is the
//!   request's and not a fixed string. The CLUSTER's own refusal of a request IAM allowed
//!   — a principal fine-grained access control binds to no role — is the engine's
//!   `security_exception` document instead, which the sibling suites
//!   [`super::domain_rest_data_plane`] and [`super::fgac_master_role_probe`] own;
//! * **changing a domain's access policy — and nothing else — re-points who may reach its
//!   data plane, and changes nothing about the domain itself**: its `DomainId`, its ARN and
//!   the address it publishes stay what they were, and the verdict each principal gets
//!   follows the policy now stored. The rows swap the two effects in both directions, so
//!   neither verdict is the one a fixed answer would give;
//! * **a statement of that access policy is about the ACTIONS it names and no others, and an
//!   explicit `Deny` among the statements that apply refuses the request whatever the
//!   principal's own identity policies allow** — so a `Deny` scoped to one `es:ESHttp*`
//!   action refuses exactly that action's requests over an identity policy allowing
//!   `es:ESHttp*` broadly, and replacing the document with one that denies nothing, the
//!   identity policies untouched, serves the identical request. The rows span the action set
//!   this surface publishes, each read through the request whose METHOD names it.
//!
//! Every case drives the real wire path — an HTTP request through the multi-service router
//! into the mounted service — and every data-plane request is sent to the address the
//! control plane PUBLISHED for the domain, dialled exactly as a client dials it. The
//! control-plane harness comes from [`super::domain_lifecycle`] and the session, policy and
//! published-address helpers from [`super::published_domain_address`]; this suite adds no
//! second copy of them.

use serde_json::{json, Value};

use super::domain_lifecycle::{settled_domain, Answer, Emulator, REGION};
use super::published_domain_address::{
    at_address, given_role_policy, given_session, published_endpoint, refusal_message, role_arn,
    session_arn,
};

/// The session name every seeded credential of this suite carries.
const SESSION: &str = "iam-session";

/// The role the domain's access policy ALLOWS, and the access key id of its session.
const ALLOWED_ROLE: &str = "plane-allowed";
const ALLOWED_KEY: &str = "ASIAEXAMPLEPLANEALW1";

/// The role that policy explicitly DENIES, and the access key id of its session.
const DENIED_ROLE: &str = "plane-denied";
const DENIED_KEY: &str = "ASIAEXAMPLEPLANEDNY1";

/// A role no statement of that policy names at all, and its session's access key id.
const UNNAMED_ROLE: &str = "plane-unnamed";
const UNNAMED_KEY: &str = "ASIAEXAMPLEPLANEUNN1";

/// A role the policy allows and whose OWN identity policy denies these actions, and its
/// session's access key id.
const SELF_DENIED_ROLE: &str = "plane-selfdenied";
const SELF_DENIED_KEY: &str = "ASIAEXAMPLEPLANESLF1";

/// An identity-based policy allowing every data-plane action of this service, which is the
/// grant a role carries so that what is left to decide is the domain's own access policy.
fn identity_allow() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": "es:ESHttp*", "Resource": "*" }],
    })
}

/// An identity-based policy that explicitly DENIES them.
fn identity_deny() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Deny", "Action": "es:ESHttp*", "Resource": "*" }],
    })
}

/// The resource an access-policy statement scopes to one whole domain: every path under that
/// domain's own ARN, which is how `AccessPolicies` names one
/// (`arn:aws:es:<region>:<account>:domain/<name>/*`).
fn domain_resource(domain: &str) -> String {
    format!(
        "arn:aws:es:{REGION}:{}:domain/{domain}/*",
        ::engine::emulator_account_id()
    )
}

/// The domain's access policy: `allowed` allowed and `denied` denied on every path of the
/// domain, with the third role named nowhere. The resource is the domain's own ARN pattern,
/// the way an access policy scopes a statement to one domain.
fn access_policy(domain: &str, allowed: &str, denied: &str) -> String {
    let resource = domain_resource(domain);
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Principal": { "AWS": role_arn(allowed) },
                "Action": "es:*",
                "Resource": resource,
            },
            {
                "Effect": "Deny",
                "Principal": { "AWS": role_arn(denied) },
                "Action": "es:*",
                "Resource": resource,
            },
        ],
    })
    .to_string()
}

/// A `CreateDomain` body for `domain` carrying `policy`.
fn create_body(domain: &str, policy: &str) -> Value {
    json!({
        "DomainName": domain,
        "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
        "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
        "DomainEndpointOptions": { "EnforceHTTPS": true },
        "AccessPolicies": policy,
    })
}

/// Seed the four sessions and the identity policies their roles carry.
fn given_principals(emulator: &Emulator) {
    for (role, key) in [
        (ALLOWED_ROLE, ALLOWED_KEY),
        (DENIED_ROLE, DENIED_KEY),
        (UNNAMED_ROLE, UNNAMED_KEY),
        (SELF_DENIED_ROLE, SELF_DENIED_KEY),
    ] {
        given_session(emulator, role, SESSION, key);
    }
    // Each role but the self-denied one carries the data-plane grant, so the decision under
    // test is the domain's own policy; the last one carries the explicit `Deny` that must
    // win over the allow that policy states about it.
    for role in [ALLOWED_ROLE, DENIED_ROLE] {
        given_role_policy(emulator, role, identity_allow());
    }
    given_role_policy(emulator, SELF_DENIED_ROLE, identity_deny());
}

/// Create the domain, wait its provisioning span out, and answer the address it published.
async fn given_domain(emulator: &Emulator, domain: &str, policy: &str, case: &str) -> String {
    let created = emulator.create(create_body(domain, policy)).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected 200 from CreateDomain, got {} with {:?}",
        created.status, created.body
    );
    // The provisioning span is the service's; what this waits for is the CONDITION a client
    // polls for — the domain reporting itself created with nothing in flight.
    settled_domain(emulator, domain).await;
    published_endpoint(emulator, domain, case).await
}

/// The identity, as a client reads it, that an in-place policy change must not move.
async fn reported_identity(emulator: &Emulator, domain: &str, case: &str) -> (Value, Value, Value) {
    let read = emulator.describe(domain).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: expected 200 from DescribeDomain, got {} with {:?}",
        read.status, read.body
    );
    let status = &read.body["DomainStatus"];
    (
        status["DomainId"].clone(),
        status["ARN"].clone(),
        status["Endpoint"].clone(),
    )
}

/// One request the IAM layer must refuse: who signs it, what it asks for, and the clause the
/// sentence ends with.
struct RefusedRow {
    case: &'static str,
    access_key_id: &'static str,
    role: &'static str,
    method: &'static str,
    path: &'static str,
    /// The `es:ESHttp*` action the method names, which the sentence must carry.
    action: &'static str,
    /// The clause naming which of IAM's rules refused.
    clause: &'static str,
}

/// The table: both explicit denies and the default deny, over the methods an `ESHttp*`
/// action is read from (`ESHttpGet`, `ESHttpPut`, `ESHttpPost`, `ESHttpDelete`,
/// `ESHttpHead`) — the actions "Actions defined by Amazon OpenSearch Service" publishes for
/// this surface.
const REFUSED_ROWS: &[RefusedRow] = &[
    RefusedRow {
        case: "a read the domain's policy explicitly denies",
        access_key_id: DENIED_KEY,
        role: DENIED_ROLE,
        method: "GET",
        path: "/catalog/_doc/1",
        action: "es:ESHttpGet",
        clause: "with an explicit deny in a resource-based policy",
    },
    RefusedRow {
        case: "a write the domain's policy explicitly denies",
        access_key_id: DENIED_KEY,
        role: DENIED_ROLE,
        method: "PUT",
        path: "/catalog/_doc/2",
        action: "es:ESHttpPut",
        clause: "with an explicit deny in a resource-based policy",
    },
    RefusedRow {
        case: "a search the domain's policy explicitly denies",
        access_key_id: DENIED_KEY,
        role: DENIED_ROLE,
        method: "POST",
        path: "/catalog/_search",
        action: "es:ESHttpPost",
        clause: "with an explicit deny in a resource-based policy",
    },
    RefusedRow {
        case: "a delete the domain's policy explicitly denies",
        access_key_id: DENIED_KEY,
        role: DENIED_ROLE,
        method: "DELETE",
        path: "/catalog/_doc/1",
        action: "es:ESHttpDelete",
        clause: "with an explicit deny in a resource-based policy",
    },
    RefusedRow {
        case: "a head request the domain's policy explicitly denies",
        access_key_id: DENIED_KEY,
        role: DENIED_ROLE,
        method: "HEAD",
        path: "/catalog/_doc/1",
        action: "es:ESHttpHead",
        clause: "with an explicit deny in a resource-based policy",
    },
    RefusedRow {
        case: "a principal the domain's policy names nowhere",
        access_key_id: UNNAMED_KEY,
        role: UNNAMED_ROLE,
        method: "GET",
        path: "/_cluster/health",
        action: "es:ESHttpGet",
        clause: "because no resource-based policy allows the es:ESHttpGet action",
    },
    RefusedRow {
        case: "a principal the domain allows whose own policy denies it",
        access_key_id: SELF_DENIED_KEY,
        role: SELF_DENIED_ROLE,
        method: "GET",
        path: "/_cluster/health",
        action: "es:ESHttpGet",
        clause: "with an explicit deny in an identity-based policy",
    },
];

/// RULE: a request IAM refuses is answered `403` with AWS's own `Message` sentence — the
/// principal that signed, the action the request's method asks for, and the rule that
/// refused — and with none of the engine's error document, because the cluster never saw it.
#[tokio::test]
async fn an_iam_refusal_is_aws_own_message_naming_the_principal_the_action_and_the_rule() {
    let emulator = Emulator::new();
    let domain = "plane-iam";
    given_principals(&emulator);
    let address = given_domain(
        &emulator,
        domain,
        &access_policy(domain, ALLOWED_ROLE, DENIED_ROLE),
        "the domain the refusals are read on",
    )
    .await;

    // The allowed principal's write is what puts the document the refused rows address into
    // the index, so none of them is refused for a document that is not there.
    let written = at_address(
        &emulator,
        &address,
        ALLOWED_KEY,
        "PUT",
        "/catalog/_doc/1",
        Some(json!({ "title": "allowed" })),
    )
    .await;
    assert_eq!(
        (written.status, written.body["result"].as_str()),
        (201, Some("created")),
        "case \"the allowed principal's write\": expected the engine's own answer, got {} with \
         {:?}",
        written.status,
        written.body
    );

    for row in REFUSED_ROWS {
        let case = row.case;
        let answered = at_address(
            &emulator,
            &address,
            row.access_key_id,
            row.method,
            row.path,
            None,
        )
        .await;
        assert_eq!(
            answered.status, 403,
            "case {case:?}: expected 403, got {} with {:?}",
            answered.status, answered.body
        );
        // A `HEAD` answer carries no body at all, so the sentence is asserted on every
        // other row; what a `HEAD` must carry is the status.
        if row.method == "HEAD" {
            continue;
        }
        let expected = format!(
            "User: {} is not authorized to perform: {} {}",
            session_arn(row.role, SESSION),
            row.action,
            row.clause
        );
        assert_eq!(
            refusal_message(&answered),
            expected,
            "case {case:?}: expected AWS's own refusal sentence, got {:?}",
            answered.body
        );
        assert_eq!(
            answered.body["error"],
            Value::Null,
            "case {case:?}: the IAM layer refuses before the cluster, so its answer carries \
             no engine error document, got {:?}",
            answered.body
        );
    }

    // And nothing the refused rows asked for happened: the document the allowed principal
    // wrote is still the only one, and it still reads back.
    let counted = at_address(
        &emulator,
        &address,
        ALLOWED_KEY,
        "GET",
        "/catalog/_count",
        None,
    )
    .await;
    assert_eq!(
        counted.body["count"].as_i64(),
        Some(1),
        "case \"after every refusal\": expected the index to hold the one document, got {} \
         with {:?}",
        counted.status,
        counted.body
    );
}

/// One swap of the two effects: which role the policy now allows and which it now denies.
struct SwapRow {
    case: &'static str,
    allowed: &'static str,
    denied: &'static str,
}

/// The table: the policy as the domain was created with it, then both effects swapped, then
/// swapped back — so each principal is observed on both sides of the boundary.
const SWAP_ROWS: &[SwapRow] = &[
    SwapRow {
        case: "the effects swapped",
        allowed: DENIED_ROLE,
        denied: ALLOWED_ROLE,
    },
    SwapRow {
        case: "the effects swapped back",
        allowed: ALLOWED_ROLE,
        denied: DENIED_ROLE,
    },
];

/// Assert the data plane serves `allowed` and refuses `denied` at `address`, as the policy
/// in force says it must.
async fn assert_verdicts(emulator: &Emulator, address: &str, row: &SwapRow, case: &str) {
    let keys = |role: &str| {
        if role == ALLOWED_ROLE {
            (ALLOWED_KEY, ALLOWED_ROLE)
        } else {
            (DENIED_KEY, DENIED_ROLE)
        }
    };
    let (allowed_key, allowed_role) = keys(row.allowed);
    let (denied_key, denied_role) = keys(row.denied);

    let served = at_address(
        emulator,
        address,
        allowed_key,
        "GET",
        "/_cluster/health",
        None,
    )
    .await;
    assert_eq!(
        served.status, 200,
        "case {case:?}: expected the cluster health for the allowed principal \
         {allowed_role:?}, got {} with {:?}",
        served.status, served.body
    );
    let refused = at_address(
        emulator,
        address,
        denied_key,
        "GET",
        "/_cluster/health",
        None,
    )
    .await;
    assert_eq!(
        (refused.status, refusal_message(&refused)),
        (
            403,
            format!(
                "User: {} is not authorized to perform: es:ESHttpGet with an explicit deny in \
                 a resource-based policy",
                session_arn(denied_role, SESSION)
            )
        ),
        "case {case:?}: expected the explicit-deny refusal for {denied_role:?}, got {} with \
         {:?}",
        refused.status,
        refused.body
    );
}

/// RULE: an access-policy-only update re-points which principals may reach the domain's data
/// plane and moves nothing else about the domain — the same `DomainId`, the same ARN and the
/// same published address — so the two verdicts swap in place.
#[tokio::test]
async fn an_access_policy_only_update_repoints_the_data_plane_in_place() {
    let emulator = Emulator::new();
    let domain = "plane-swap";
    given_principals(&emulator);
    let address = given_domain(
        &emulator,
        domain,
        &access_policy(domain, ALLOWED_ROLE, DENIED_ROLE),
        "the domain the swap is run on",
    )
    .await;
    let identity = reported_identity(&emulator, domain, "before the first update").await;
    assert_verdicts(
        &emulator,
        &address,
        &SwapRow {
            case: "as created",
            allowed: ALLOWED_ROLE,
            denied: DENIED_ROLE,
        },
        "as created",
    )
    .await;

    for row in SWAP_ROWS {
        let case = row.case;
        let policy = access_policy(domain, row.allowed, row.denied);
        let updated = emulator
            .update_config(domain, json!({ "AccessPolicies": policy.clone() }))
            .await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: expected 200 from UpdateDomainConfig, got {} with {:?}",
            updated.status, updated.body
        );
        // The change the update started is the clock's to settle; the domain keeps serving
        // through it, and the policy now stored is what every later request is decided by.
        // The landing is polled for rather than waited out.
        settled_domain(&emulator, domain).await;

        let read = emulator.describe(domain).await;
        let stored = read.body["DomainStatus"]["AccessPolicies"]
            .as_str()
            .unwrap_or_default();
        assert_eq!(
            serde_json::from_str::<Value>(stored).unwrap_or(Value::Null),
            serde_json::from_str::<Value>(&policy).unwrap_or(Value::Null),
            "case {case:?}: expected the document the update sent to be the one in force, got \
             {stored:?}"
        );
        assert_eq!(
            reported_identity(&emulator, domain, case).await,
            identity,
            "case {case:?}: an access-policy update must move neither the domain's identity \
             nor the address it publishes"
        );
        assert_eq!(
            published_endpoint(&emulator, domain, case).await,
            address,
            "case {case:?}: expected the published address to stay what a client stored"
        );
        assert_verdicts(&emulator, &address, row, case).await;
    }
}

/// RULE (the same one, read from the other side): the address a domain publishes is the one
/// its own requests are served at, whichever endpoint the control-plane read reached — so a
/// caller that read the address from one endpoint and dialled it reaches the same domain.
#[tokio::test]
async fn the_published_address_serves_the_domain_whichever_endpoint_read_it() {
    let emulator = Emulator::new();
    given_principals(&emulator);
    // Two domains, so an address that reached "a domain" could not be the one domain a
    // single-domain store leaves no choice about.
    let mut addresses = Vec::new();
    for domain in ["plane-first", "plane-second"] {
        let created = emulator
            .create(create_body(
                domain,
                &access_policy(domain, ALLOWED_ROLE, DENIED_ROLE),
            ))
            .await;
        assert_eq!(
            created.status, 200,
            "case {domain:?}: expected 200 from CreateDomain, got {} with {:?}",
            created.status, created.body
        );
        addresses.push(domain);
    }

    for domain in addresses {
        settled_domain(&emulator, domain).await;
        let address = published_endpoint(&emulator, domain, domain).await;
        let banner: Answer = at_address(&emulator, &address, ALLOWED_KEY, "GET", "/", None).await;
        assert_eq!(
            (
                banner.status,
                banner.body["cluster_name"].as_str().unwrap_or_default()
            ),
            (
                200,
                format!("{}:{domain}", ::engine::emulator_account_id()).as_str()
            ),
            "case {domain:?}: expected the address to serve this domain's own cluster, got {} \
             with {:?}",
            banner.status,
            banner.body
        );
    }
}

/// The role the action-scoped rows below are decided for, and the access key id of its
/// session.
///
/// Its own identity policy allows every data-plane action of this service broadly
/// ([`identity_allow`]), so the only thing left to decide about its requests is the domain's
/// own access policy — which is the boundary these rows are about.
const SCOPED_ROLE: &str = "plane-scoped";
const SCOPED_KEY: &str = "ASIAEXAMPLEPLANESCP1";

/// The index the action probes address.
const SCOPED_INDEX: &str = "catalog";

/// Seed the session the action-scoped rows sign with, and the broad identity grant its role
/// carries.
fn given_scoped_principal(emulator: &Emulator) {
    given_session(emulator, SCOPED_ROLE, SESSION, SCOPED_KEY);
    given_role_policy(emulator, SCOPED_ROLE, identity_allow());
}

/// An access policy that allows [`SCOPED_ROLE`] every data-plane action on every path of
/// `domain` and explicitly DENIES it the single action `denied` on the same resource.
fn action_scoped_policy(domain: &str, denied: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Principal": { "AWS": role_arn(SCOPED_ROLE) },
                "Action": "es:ESHttp*",
                "Resource": domain_resource(domain),
            },
            {
                "Effect": "Deny",
                "Principal": { "AWS": role_arn(SCOPED_ROLE) },
                "Action": denied,
                "Resource": domain_resource(domain),
            },
        ],
    })
    .to_string()
}

/// The SAME document with the `Deny` statement removed and nothing else changed: the repaired
/// access policy a client replaces a wrongly denying one with.
fn repaired_policy(domain: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": role_arn(SCOPED_ROLE) },
            "Action": "es:ESHttp*",
            "Resource": domain_resource(domain),
        }],
    })
    .to_string()
}

/// One request of this surface, and the `es:ESHttp*` action its HTTP METHOD names.
struct ActionProbe {
    /// The action the request asks for, as an access-policy statement spells it.
    action: &'static str,
    method: &'static str,
    path: &'static str,
    /// The request body, as JSON text, for the methods that carry one.
    body: Option<&'static str>,
}

/// The probes the action-scoped table is read over: one request per action of this surface,
/// each the action its method names.
///
/// The action set is the one "Actions defined by Amazon OpenSearch Service" publishes for the
/// HTTP surface of a domain (`es:ESHttpGet`, `es:ESHttpPut`, `es:ESHttpPost`,
/// `es:ESHttpDelete`, `es:ESHttpHead`) — the same set the sibling rows above read a method's
/// action from, including `es:ESHttpHead`, which the issue the scoped rows were written for
/// never cited.
const ACTION_PROBES: &[ActionProbe] = &[
    ActionProbe {
        action: "es:ESHttpGet",
        method: "GET",
        path: "/catalog/_doc/probe",
        body: None,
    },
    ActionProbe {
        action: "es:ESHttpPut",
        method: "PUT",
        path: "/catalog/_doc/probe",
        body: Some(r#"{"title":"probe"}"#),
    },
    ActionProbe {
        action: "es:ESHttpPost",
        method: "POST",
        path: "/catalog/_search",
        body: Some("{}"),
    },
    ActionProbe {
        action: "es:ESHttpDelete",
        method: "DELETE",
        path: "/catalog/_doc/probe",
        body: None,
    },
    ActionProbe {
        action: "es:ESHttpHead",
        method: "HEAD",
        path: "/catalog",
        body: None,
    },
];

/// Send one probe to `address` as the action-scoped role's session.
async fn probed(emulator: &Emulator, address: &str, probe: &ActionProbe) -> Answer {
    let body = probe.body.map(|body| {
        serde_json::from_str::<Value>(body)
            .unwrap_or_else(|failed| panic!("a probe body is JSON: {body:?} ({failed})"))
    });
    at_address(
        emulator,
        address,
        SCOPED_KEY,
        probe.method,
        probe.path,
        body,
    )
    .await
}

/// The sentence a resource-policy `Deny` of `action` refuses the scoped role's request with.
fn scoped_deny_sentence(action: &str) -> String {
    format!(
        "User: {} is not authorized to perform: {action} {EXPLICIT_RESOURCE_DENY}",
        session_arn(SCOPED_ROLE, SESSION)
    )
}

/// The clause IAM's refusal names when a RESOURCE-based policy explicitly denied the request.
const EXPLICIT_RESOURCE_DENY: &str = "with an explicit deny in a resource-based policy";

/// The ONE write the deny-and-repair rule is read through, sent as the scoped role's session:
/// the identical request before and after the access policy is replaced.
async fn scoped_write(emulator: &Emulator, address: &str) -> Answer {
    at_address(
        emulator,
        address,
        SCOPED_KEY,
        "PUT",
        &format!("/{SCOPED_INDEX}/_doc/scoped"),
        Some(json!({ "title": "scoped" })),
    )
    .await
}

/// Install `policy` as the domain's access policy through `UpdateDomainConfig`, and wait for
/// the change it starts to land — the condition a client polls for, never a fixed stretch of
/// time.
async fn given_access_policy(emulator: &Emulator, domain: &str, policy: &str, case: &str) {
    let updated = emulator
        .update_config(domain, json!({ "AccessPolicies": policy }))
        .await;
    assert_eq!(
        updated.status, 200,
        "case {case:?}: expected 200 from UpdateDomainConfig, got {} with {:?}",
        updated.status, updated.body
    );
    settled_domain(emulator, domain).await;
}

/// RULE: an explicit `Deny` in the domain's access policy refuses the request whatever the
/// principal's identity policies allow, and a statement scoped to ONE action says nothing
/// about any other — so a document that denies the write refuses exactly the write, and
/// replacing it with one that denies nothing, the identity policies untouched, serves the
/// identical write.
#[tokio::test]
async fn a_resource_policy_deny_of_one_action_outranks_a_broad_identity_allow() {
    let emulator = Emulator::new();
    given_scoped_principal(&emulator);
    let domain = "plane-scoped";
    let created = emulator
        .create(create_body(
            domain,
            &action_scoped_policy(domain, "es:ESHttpPut"),
        ))
        .await;
    assert_eq!(
        created.status, 200,
        "case \"the deny-scoped domain\": expected 200 from CreateDomain, got {} with {:?}",
        created.status, created.body
    );
    settled_domain(&emulator, domain).await;
    let address = published_endpoint(&emulator, domain, "the deny-scoped domain").await;

    // The write the access policy denies, made by a principal whose own policy allows
    // `es:ESHttp*` on everything: refused, and named as the resource side's deny.
    let refused = scoped_write(&emulator, &address).await;
    assert_eq!(
        (refused.status, refusal_message(&refused)),
        (403, scoped_deny_sentence("es:ESHttpPut")),
        "case \"the denied write\": expected the resource-policy deny of es:ESHttpPut, got {} \
         with {:?}",
        refused.status,
        refused.body
    );
    // The statement is about ONE action, so what it does not name is not refused.
    let health = at_address(
        &emulator,
        &address,
        SCOPED_KEY,
        "GET",
        "/_cluster/health",
        None,
    )
    .await;
    assert_eq!(
        health.status, 200,
        "case \"a read the same policy does not deny\": expected the cluster health, got {} \
         with {:?}",
        health.status, health.body
    );
    // And the refused write wrote nothing: the index it addressed does not exist.
    let absent = at_address(
        &emulator,
        &address,
        SCOPED_KEY,
        "GET",
        &format!("/{SCOPED_INDEX}/_doc/scoped"),
        None,
    )
    .await;
    assert_eq!(
        absent.status, 404,
        "case \"after the denied write\": expected the document never to have been written, \
         got {} with {:?}",
        absent.status, absent.body
    );

    // THE REPAIR: the same document with the `Deny` statement taken out. Nothing about the
    // principal's identity policies is touched — the grant seeded once above is still the
    // only one in force on it.
    given_access_policy(
        &emulator,
        domain,
        &repaired_policy(domain),
        "the repaired access policy",
    )
    .await;

    let written = scoped_write(&emulator, &address).await;
    assert_eq!(
        (written.status, written.body["result"].as_str()),
        (201, Some("created")),
        "case \"the identical write after the repair\": expected the engine's own answer, got \
         {} with {:?}",
        written.status,
        written.body
    );
    let read_back = at_address(
        &emulator,
        &address,
        SCOPED_KEY,
        "GET",
        &format!("/{SCOPED_INDEX}/_doc/scoped"),
        None,
    )
    .await;
    assert_eq!(
        (
            read_back.status,
            read_back.body["_source"]["title"].as_str()
        ),
        (200, Some("scoped")),
        "case \"reading back what the repaired policy let through\": expected the document, \
         got {} with {:?}",
        read_back.status,
        read_back.body
    );
}

/// RULE (the same one over the whole action set): a `Deny` scoped to one action of this
/// surface refuses the requests whose METHOD names that action and leaves every other action
/// of the same principal served, with the identity grant unchanged throughout.
#[tokio::test]
async fn a_deny_scoped_to_one_action_refuses_that_action_alone() {
    let emulator = Emulator::new();
    given_scoped_principal(&emulator);
    let domain = "plane-scoped-actions";
    let created = emulator
        .create(create_body(domain, &repaired_policy(domain)))
        .await;
    assert_eq!(
        created.status, 200,
        "case \"the action-scoped domain\": expected 200 from CreateDomain, got {} with {:?}",
        created.status, created.body
    );
    settled_domain(&emulator, domain).await;
    let address = published_endpoint(&emulator, domain, "the action-scoped domain").await;

    // The index every probe addresses, written while the policy denies nothing, so a probe's
    // status is the authorization decision and not an index that was never there.
    let seeded = at_address(
        &emulator,
        &address,
        SCOPED_KEY,
        "PUT",
        &format!("/{SCOPED_INDEX}/_doc/probe"),
        Some(json!({ "title": "probe" })),
    )
    .await;
    assert!(
        (200..300).contains(&seeded.status),
        "case \"seeding the probed index\": expected the write to land, got {} with {:?}",
        seeded.status,
        seeded.body
    );

    for denied in ACTION_PROBES {
        let case = denied.action;
        given_access_policy(
            &emulator,
            domain,
            &action_scoped_policy(domain, denied.action),
            case,
        )
        .await;

        for probe in ACTION_PROBES {
            let probed_action = probe.action;
            let answered = probed(&emulator, &address, probe).await;
            if probed_action == denied.action {
                assert_eq!(
                    answered.status, 403,
                    "case {case:?}: expected the denied action {probed_action:?} to be \
                     refused, got {} with {:?}",
                    answered.status, answered.body
                );
                // A `HEAD` answer carries no body at all, so the sentence is asserted on
                // every other probe; what a `HEAD` must carry is the status.
                if probe.method != "HEAD" {
                    assert_eq!(
                        refusal_message(&answered),
                        scoped_deny_sentence(probed_action),
                        "case {case:?}: expected AWS's own resource-deny sentence for \
                         {probed_action:?}, got {:?}",
                        answered.body
                    );
                }
            } else {
                assert_ne!(
                    answered.status, 403,
                    "case {case:?}: a statement about {case:?} says nothing about \
                     {probed_action:?}, which the identity grant allows, got {} with {:?}",
                    answered.status, answered.body
                );
            }
        }
    }
}
