// SPDX-License-Identifier: CC0-1.0

//! How an authorization QUESTION about an IoT principal is decided: which policies are in
//! effect for it, and what one action on one resource comes out of them.
//!
//! An IoT policy document is an IAM-grammar JSON policy — that is what
//! [`super::validate_policy_document`] accepts — so the DECISION is the tree's one
//! evaluation of such a document against a request,
//! [`::service_common::iam_authorization`]: a statement is about the request only when its
//! action element names the action and its resource element matches the resource, an
//! explicit `Deny` among those wins over every `Allow`, and documents that state nothing
//! about the request neither grant nor refuse. Conditions, policy variables and the two
//! IAM wildcards are that module's to read, so an IoT decision and an IAM one cannot
//! disagree about the same document. Nothing of that rule is restated here.
//!
//! What IS IoT's own, and therefore lives here:
//!
//! * which policy action each `ActionType` asks about
//!   ([`iot_action_for_action_type`]) — the one place the enum and the IAM action names
//!   meet. The resource NAMESPACES follow from it with no rule of their own: a
//!   subscription is authorized against a `topicfilter/<filter>` ARN and a publication or
//!   a delivery against a `topic/<topic>` ARN, two different strings, so a grant written in
//!   one namespace never matches a question asked in the other;
//! * which policies are in effect for a principal
//!   ([`super::ComAmazonawsIotAwsiotserviceState::policies_in_effect`]): the attachment
//!   relation's, under the two name lists `TestAuthorization` can override them with;
//! * which condition keys the device gateway's authorizer ANSWERS for the connection being
//!   simulated ([`SimulatedConnection`]), and therefore which keys it cannot answer for at
//!   all — the ones a decision reports under `AuthResult$missingContextValues`;
//! * how a decision over SEVERAL resources, and the attribution of it to the policies it
//!   came from, becomes one `AuthResult` ([`evaluate_authorization`]).
//!
//! Deciding READS the policy registry and the attachment relation and records nothing, so
//! the same question asked twice of unchanged state answers the same way.

use std::collections::BTreeSet;

use ::service_common::iam_authorization::{
    identity_outcome_prepared, prepare_documents, Decision, Request,
};

/// The `AuthDecision` of a question a statement allows and none explicitly denies.
pub const AUTH_DECISION_ALLOWED: &str = "ALLOWED";

/// The `AuthDecision` of a question a statement explicitly denies, whatever else allows
/// it.
pub const AUTH_DECISION_EXPLICIT_DENY: &str = "EXPLICIT_DENY";

/// The `AuthDecision` of a question no statement in effect states anything about.
pub const AUTH_DECISION_IMPLICIT_DENY: &str = "IMPLICIT_DENY";

/// The condition key a policy variable names the MQTT client id of the connection being
/// authorized by: the value `TestAuthorization$clientId` ("The MQTT client ID") supplies.
pub const CLIENT_ID_CONTEXT_KEY: &str = "iot:ClientId";

/// The condition key carrying the ADDRESS a connection is made from: "`aws:SourceIp` — the
/// requester's IP address", which is what an address clause compares against.
///
/// Every connection arrives from an address, so the authorizer answers this key for every
/// authorization it decides — which is why a clause over it is DECIDED here rather than
/// left for a caller to supply a value for.
pub const SOURCE_IP_CONTEXT_KEY: &str = "aws:SourceIp";

/// The header a proxy in front of this emulator states the address it forwarded a request
/// for in, whose FIRST entry is the client's own — the same reading of a request's source
/// address every other service in this tree takes.
const FORWARDED_FOR_HEADER: &str = "x-forwarded-for";

/// The address a request carries when nothing forwarded it: this emulator serves its API
/// over the loopback interface, so a connection that reached it came from the local host.
const LOOPBACK_SOURCE_IP: &str = "127.0.0.1";

/// The condition context ONE authorization question is decided in: the keys the device
/// gateway's authorizer ANSWERS for the connection being simulated, each with the value it
/// answers.
///
/// The authorizer answers for the connection it is deciding about — the MQTT client id the
/// request named ([`CLIENT_ID_CONTEXT_KEY`]) and the address the request was made from
/// ([`SOURCE_IP_CONTEXT_KEY`]) — and for nothing else. So a key that is HERE is compared
/// against the value it carries, which makes a clause it does not satisfy a plain non-match
/// naming no missing key; and a key that is NOT here is one the authorizer cannot answer for
/// this request at all (`aws:SourceVpce`, for a request that did not arrive over a VPC
/// endpoint), which leaves a statement conditioned on it undecidable — neither a grant nor a
/// refusal — and is what a decision reports under `AuthResult$missingContextValues`.
#[derive(Debug, Clone, Default)]
pub struct SimulatedConnection {
    /// The `(key, value)` pairs, in no significant order: the evaluation compares a
    /// condition key's name case-insensitively.
    entries: Vec<(String, String)>,
}

impl SimulatedConnection {
    /// The connection a `TestAuthorization` request simulates: the MQTT client id it names,
    /// where it names one, and the address that request itself was made from — which is the
    /// one address an authorization asked for over this API can be decided against.
    pub fn of(client_id: Option<&str>, parts: &::http::request::Parts) -> Self {
        Self::new(client_id, &source_address(parts))
    }

    /// The same context from the address itself.
    fn new(client_id: Option<&str>, source_ip: &str) -> Self {
        let mut entries = vec![(SOURCE_IP_CONTEXT_KEY.to_string(), source_ip.to_string())];
        // A request naming no client id is one the authorizer has no client id to answer
        // with, so the key stays out of the context rather than being answered with an
        // empty value: a statement conditioned on it is then undecidable and the key is
        // reported, exactly as every other key the authorizer cannot answer for is.
        if let Some(client_id) = client_id {
            entries.push((CLIENT_ID_CONTEXT_KEY.to_string(), client_id.to_string()));
        }
        Self { entries }
    }

    /// The pairs a condition clause and a policy variable are read against.
    fn context(&self) -> &[(String, String)] {
        &self.entries
    }

    /// The keys the authorizer ANSWERS for this connection, which is what the evaluation is
    /// told it models: the absence of any OTHER key means the authorizer cannot answer for
    /// it rather than that the connection carries no value for it.
    fn answered_keys(&self) -> Vec<&str> {
        self.entries.iter().map(|(key, _)| key.as_str()).collect()
    }
}

/// The address the request `parts` was made from, as [`SOURCE_IP_CONTEXT_KEY`] carries it.
///
/// Read off the CONNECTION and never off the request's own parameters: the first entry of
/// [`FORWARDED_FOR_HEADER`] where a proxy stated the client it forwarded for, and otherwise
/// [`LOOPBACK_SOURCE_IP`], the host every connection to this emulator is made from. So a
/// statement pinned to a network either admits or refuses the question rather than leaving
/// it undecided.
fn source_address(parts: &::http::request::Parts) -> String {
    parts
        .headers
        .get(FORWARDED_FOR_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|address| !address.is_empty())
        .unwrap_or(LOOPBACK_SOURCE_IP)
        .to_string()
}

/// How many resources ONE question may name.
///
/// `AuthInfo$resources` carries no modelled bound, while `authInfos` is modelled
/// `@length(min: 1, max: 10)`, so without this one request could ask for unbounded
/// evaluation work: the cost of a decision is the number of questions times the resources
/// each names times the statements in effect. This is the emulator's own ceiling on the
/// middle factor — the reason [`super::MAX_POLICIES_PER_TARGET`] exists for the relation —
/// and a question past it is refused with the modelled `LimitExceededException` (HTTP 410)
/// naming the limit rather than evaluated.
pub const MAX_AUTHORIZED_RESOURCES: usize = 10;

/// The policy action one `ActionType` asks about, or `None` when the value is not one of
/// the four the enum carries.
///
/// `ActionType` is "The type of action for which the principal is being authorized"
/// (PUBLISH | SUBSCRIBE | RECEIVE | CONNECT) and an IoT policy grants the action by its
/// IAM name, so this mapping is the one place the two namings meet.
pub fn iot_action_for_action_type(action_type: &str) -> Option<&'static str> {
    match action_type {
        "CONNECT" => Some("iot:Connect"),
        "PUBLISH" => Some("iot:Publish"),
        "RECEIVE" => Some("iot:Receive"),
        "SUBSCRIBE" => Some("iot:Subscribe"),
        _ => None,
    }
}

/// One policy whose statements decide a question: its identity, and the document of the
/// version in effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyInEffect {
    /// The name the registry holds the policy under.
    pub policy_name: String,
    /// The ARN minted for it when it was created.
    pub policy_arn: String,
    /// The document of the version in effect — the one `GetPolicy` reports.
    pub document: String,
}

/// How a decision names one of the policies that produced it: the `Policy` shape's pair
/// of members, which is all a reader of a decision is told about a policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyReference {
    /// `Policy$policyName`.
    pub policy_name: String,
    /// `Policy$policyArn`.
    pub policy_arn: String,
}

/// The answer to one authorization question: the decision, and which policies produced
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationDecision {
    /// [`AUTH_DECISION_ALLOWED`], [`AUTH_DECISION_EXPLICIT_DENY`] or
    /// [`AUTH_DECISION_IMPLICIT_DENY`].
    pub decision: &'static str,
    /// The policies that allowed it — "A list of policies that allowed the
    /// authentication", so it is populated only when the decision IS allowed.
    pub allowed: Vec<PolicyReference>,
    /// "The policies that denied the authorization", populated only when a statement
    /// denied it.
    pub explicit_deny: Vec<PolicyReference>,
    /// "Policies that don't contain a matching allow or deny statement for the specified
    /// action on the specified resource", populated only when the decision is the implicit
    /// deny.
    pub implicit_deny: Vec<PolicyReference>,
    /// Every condition key a statement about the question is conditioned on and the
    /// authorizer does not answer for the simulated connection, once each and in one
    /// stable order (`AuthResult$missingContextValues`). A key it DOES answer for is
    /// compared against its value, so a clause that value does not satisfy is a plain
    /// non-match and is named here by nothing.
    pub missing_context_values: Vec<String>,
}

/// Decide whether `policies` let the principal they are in effect for perform `action` on
/// every one of `resources`, for the `connection` being simulated.
///
/// The question is about the resources TOGETHER — `AuthInfo` pairs one `actionType` with
/// the list of "resources for which the principal is being authorized to perform the
/// specified action" and one `AuthResult` answers it — so it is allowed only when every
/// resource is, denied when a statement denies any of them, and allowed on none of them
/// when it names no resource at all.
///
/// A policy whose statement about the question is conditioned on a key the authorizer does
/// not answer for this connection ([`SimulatedConnection`]) DECIDES NOTHING about it: the
/// clause can be read neither as holding nor as failing, so such a statement neither grants
/// the question nor refuses it, whichever effect it states. The question then falls to the
/// implicit deny — a refusal a further grant can fix, not one a `Deny` statement
/// pronounced — and the keys those clauses named are what the decision reports as missing,
/// which is how a caller is told to supply the key rather than to rewrite the policy.
///
/// The policies are attributed the way the IAM policy simulator attributes them: a
/// decision names the policies whose statements it was reached from, and the implicit deny
/// names the policies that stated nothing the question could be decided from — those with
/// no matching statement, and those whose matching statement could not be decided.
pub fn evaluate_authorization(
    policies: &[PolicyInEffect],
    action: Option<&str>,
    resources: &[String],
    connection: &SimulatedConnection,
) -> AuthorizationDecision {
    // Read once, then asked per resource: one statement walk per question instead of one
    // parse per question.
    let documents: Vec<String> = policies
        .iter()
        .map(|policy| policy.document.clone())
        .collect();
    let documents = prepare_documents(&documents);

    // Which policies denied, which allowed, which stated anything at all, and which stated
    // something this evaluation could not decide, by their position in `policies`.
    let mut denying: BTreeSet<usize> = BTreeSet::new();
    let mut allowing: BTreeSet<usize> = BTreeSet::new();
    let mut stating: BTreeSet<usize> = BTreeSet::new();
    let mut undecidable: BTreeSet<usize> = BTreeSet::new();
    let mut missing_context_values: BTreeSet<String> = BTreeSet::new();
    let mut allowed_resources = 0usize;

    let context = connection.context();
    let answered = connection.answered_keys();

    // No `actionType` names no action, so no statement is about the question at all: there
    // is nothing a grant of an action could name, and every policy in effect states
    // nothing about it.
    let asked = action.into_iter().flat_map(|action| {
        resources
            .iter()
            .map(move |resource| (action, resource.as_str()))
    });
    for (action, resource) in asked {
        // The authorizer answers for the keys of the connection it is deciding about and
        // for no others, which is exactly what the evaluation is told here: a clause over
        // an answered key is compared against its value, and one over any other key is
        // left undecided and its key reported.
        let request = Request::new(action, resource, &[])
            .with_context(context)
            .with_published_keys(&answered);
        // Each policy is asked on its own, so what one policy's statements decided — and
        // whether they could be decided at all — is known per policy rather than folded
        // into one answer for the whole set.
        let mut granted = false;
        let mut refused = false;
        for (position, document) in documents.iter().enumerate() {
            let outcome = identity_outcome_prepared(::std::slice::from_ref(document), &request);
            if !outcome.required_context_keys.is_empty() {
                // A statement of this policy is about the question and names a key the
                // authorizer does not answer for: the policy decides nothing about this
                // resource, and the key is what the caller is told to supply.
                //
                // Read BEFORE the decision, because a refusal this policy reports may be the
                // undecidable statement's own: the shared evaluation keeps a `Deny` whose
                // condition it could not read, which is exactly the statement this question
                // has no verdict for. Which of a policy's statements could not be decided is
                // not reported, so a policy that names such a key is read as deciding
                // nothing — the conservative reading, and the one that distinguishes a
                // refusal a further grant fixes from one a `Deny` pronounced.
                missing_context_values.extend(outcome.required_context_keys);
                undecidable.insert(position);
                continue;
            }
            if outcome.matched.is_empty() {
                continue;
            }
            // A decision is reached from the DENYING statements when it denies and from the
            // allowing ones when it allows, so which side of the question this policy is on
            // is read off the statements the walk attributed its decision to.
            stating.insert(position);
            match outcome.decision {
                Decision::Denied => {
                    denying.insert(position);
                    refused = true;
                }
                Decision::Allowed => {
                    allowing.insert(position);
                    granted = true;
                }
                Decision::NotStated => {}
            }
        }
        if granted && !refused {
            allowed_resources += 1;
        }
    }

    let every_resource_allowed = !resources.is_empty() && allowed_resources == resources.len();
    let decision = if !denying.is_empty() {
        AUTH_DECISION_EXPLICIT_DENY
    } else if undecidable.is_empty() && every_resource_allowed {
        // An undecidable statement withholds the grant: it may be a `Deny` whose condition
        // would have held, so a question it bears on is never reported as allowed.
        AUTH_DECISION_ALLOWED
    } else {
        AUTH_DECISION_IMPLICIT_DENY
    };

    let reference = |position: &usize| -> Option<PolicyReference> {
        policies.get(*position).map(|policy| PolicyReference {
            policy_name: policy.policy_name.clone(),
            policy_arn: policy.policy_arn.clone(),
        })
    };

    AuthorizationDecision {
        decision,
        allowed: if decision == AUTH_DECISION_ALLOWED {
            allowing.iter().filter_map(reference).collect()
        } else {
            Vec::new()
        },
        explicit_deny: denying.iter().filter_map(reference).collect(),
        implicit_deny: if decision == AUTH_DECISION_IMPLICIT_DENY {
            // The policies that contain no matching allow or deny statement for this
            // action on these resources — those none of the walks read a statement from —
            // and those whose matching statement could not be decided, which left the
            // question without one.
            (0..policies.len())
                .filter(|position| undecidable.contains(position) || !stating.contains(position))
                .collect::<Vec<usize>>()
                .iter()
                .filter_map(reference)
                .collect()
        } else {
            Vec::new()
        },
        missing_context_values: missing_context_values.into_iter().collect(),
    }
}

impl super::ComAmazonawsIotAwsiotserviceState {
    /// The policies whose statements decide a question asked about a principal: those the
    /// attachment relation holds for every key of `scope`, plus the names
    /// `treat_as_attached` and minus the names `treat_as_detached`.
    ///
    /// The two name lists are what `TestAuthorization` documents: `policyNamesToAdd` are
    /// "treated as if they are attached to the principal being authorized" and
    /// `policyNamesToSkip` "as if they are not", so a simulated decision can be asked
    /// about a policy set that is not the one the relation holds — without the relation
    /// being touched.
    ///
    /// Each policy is reported ONCE however many paths bring it into effect, with the
    /// document of the version in effect, which is the document `GetPolicy` reports. A
    /// name the registry does not hold decides nothing and is left out rather than
    /// carried without a document.
    pub fn policies_in_effect(
        &self,
        scope: &[String],
        treat_as_attached: &[String],
        treat_as_detached: &[String],
    ) -> Vec<PolicyInEffect> {
        let mut names: BTreeSet<String> = BTreeSet::new();
        for key in scope {
            names.extend(self.policy_attachments.policies_of(key));
        }
        names.extend(treat_as_attached.iter().cloned());
        for name in treat_as_detached {
            names.remove(name);
        }
        names
            .iter()
            .filter_map(|policy_name| self.policies.get(policy_name))
            .map(|policy| PolicyInEffect {
                policy_name: policy.policy_name.clone(),
                policy_arn: policy.policy_arn.clone(),
                document: policy.default_document().to_string(),
            })
            .collect()
    }
}
