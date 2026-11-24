// SPDX-License-Identifier: CC0-1.0

//! The WAFv2 WEB ACL in front of a distribution: how one viewer request is decided before
//! the edge reads its cache or reaches an origin.
//!
//! `DistributionConfig$WebACLId` is "a unique identifier that specifies the WAF web ACL, if
//! any, to associate with this distribution. To specify a web ACL created using the latest
//! version of WAF, use the ACL ARN" — and the association is the DISTRIBUTION's own, which
//! is why WAFv2's `AssociateWebACL` documents "for Amazon CloudFront, don't use this call.
//! Instead, use your CloudFront distribution configuration". So this module resolves the web
//! ACL a distribution's stored configuration names and asks WAFv2 about the request:
//! "CloudFront responds to requests either with the requested content or with an HTTP 403
//! status code (Forbidden)", and a refusal is answered at the edge, before the cache is read
//! and with no origin request made.
//!
//! The decision itself is WAFv2's own
//! ([`service_com_amazonaws_wafv2_awswaf_20190729::service::edge::decide_against`]): this
//! module holds no second reading of a rule tree. It does the two things only this service
//! can do — name WHICH web ACL the distribution put in front of itself, and read the viewer
//! request into the components a `FieldToMatch` names — and says what the edge answers.
//!
//! The same resolution is what an ASSOCIATION is admitted by ([`associable`]), and what a
//! write carrying a whole `DistributionConfig` is admitted by
//! ([`inaccessible_web_acl`]): the web ACL a CloudFront resource is put behind has to be one
//! WAF holds, however the configuration naming it arrived, so the one module that asks WAF
//! about a distribution's web ACL is where both questions are asked.

use aws_smithy_runtime_api::http::Request as RtRequest;
use service_com_amazonaws_wafv2_awswaf_20190729::service::edge as waf_edge;
use service_com_amazonaws_wafv2_awswaf_20190729::service::logs as waf_logs;

/// The status a terminating action is answered under when the action names none —
/// `EdgeAction::default_status` answers one for every terminating action, so this is only
/// the fallback, and it is the status CloudFront's own documentation names for a request a
/// web ACL does not let through (`403`).
const EDGE_REFUSAL_STATUS: u16 = 403;

/// What the edge answers for a viewer request the web ACL in front of the distribution did
/// not let through.
#[derive(Debug)]
pub struct Refusal {
    /// The status the refusal is answered under.
    pub status: u16,
    /// The media type of [`Self::body`], when the web ACL's custom response names one.
    pub content_type: Option<String>,
    /// The body the web ACL's custom response serves, empty when it configures none.
    pub body: Vec<u8>,
    /// The headers the web ACL's custom response configures, in that order.
    pub headers: Vec<(String, String)>,
}

/// What the web ACL `web_acl_id` names decides about the viewer request `req`, whose path
/// within the distribution's endpoint is `viewer_path` and whose query string (without its
/// leading `?`) is `query`, beside the record that web ACL's own `LoggingConfiguration`
/// logs about the request.
///
/// A `None` refusal lets the request through: the distribution names no web ACL, names one
/// by an identifier that is no WAFv2 ARN (a WAF Classic ACL id, whose rules this emulator
/// holds none of), names one this emulator does not hold, or the web ACL allowed it.
///
/// The record is WAFv2's ([`waf_logs`]); this service only DELIVERS it, with
/// `waf_logs::deliver`, because the write is a call made from the operation serving the
/// request. `None` when the web ACL logs nothing about this request.
///
/// The two stores are locked one after the other and never at the same time, and the wafv2
/// side locks nothing of this service's, so the two cannot deadlock: the caller releases
/// this service's store before asking, and the decision reads its own records under a
/// shared lock, evaluates with the lock released, writes its data plane's own bookkeeping
/// under an exclusive one, and publishes the `AWS/WAFV2` counters once that lock is gone.
pub fn refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    web_acl_id: &str,
    req: &RtRequest,
    viewer_path: &str,
    query: &str,
) -> (Option<Refusal>, Option<waf_logs::LoggedRecord>) {
    if !names_a_web_acl_arn(web_acl_id) {
        return (None, None);
    }
    // The body as it arrived, which is what a rule inspecting `FieldToMatch$Body` reads. A
    // viewer request this plane serves is one the front door handed over whole, so its body
    // is in memory; a body that cannot be read as bytes runs against no body rather than
    // being quietly inspected as an empty one.
    let body = req.body().bytes().map(<[u8]>::to_vec).unwrap_or_default();
    let inspected = waf_edge::InspectedRequest::of_http(req, viewer_path, query, body);
    let Some(decided) = waf_edge::decide_against(engine, web_acl_id, &inspected) else {
        return (None, None);
    };
    let logged = decided.log_record.clone();
    if decided.action.is_allow() {
        return (None, logged);
    }
    ::tracing::debug!(
        web_acl = %web_acl_id,
        rule = decided.rule_name.as_deref().unwrap_or("<default action>"),
        "the web acl in front of this distribution does not let the request through"
    );
    // A terminating action that configures its own `CustomResponse` is served BY WAF: the
    // status, the headers and the body are the web ACL's own, and the edge produced none of
    // them. Either way the answer carries the headers WAF stamps on a response it generates
    // itself — `EdgeDecision::response_headers`, which names the action of a `Captcha` or
    // `Challenge` interception.
    let stamped = decided.response_headers;
    let refusal = match decided.custom_response {
        Some(custom) => Refusal {
            status: custom.status,
            content_type: custom.content_type,
            body: custom.body,
            headers: custom.headers.into_iter().chain(stamped).collect(),
        },
        None => Refusal {
            status: decided
                .action
                .default_status()
                .unwrap_or(EDGE_REFUSAL_STATUS),
            content_type: None,
            body: Vec::new(),
            headers: stamped,
        },
    };
    (Some(refusal), logged)
}

/// Whether a web ACL `web_acl_arn` names may be put in front of a CloudFront resource:
/// whether WAF really holds one under that ARN, and whether it is one of the namespace
/// CloudFront is protected by ([`crate::state::associable_web_acl_arn`]).
///
/// What an ASSOCIATION resolves before it writes anything. A web ACL is a WAF resource, so
/// whether the account HAS the one a request names is a question only WAF can answer, and
/// CloudFront asks it: "specify a web ACL that was previously validated" is what it tells a
/// caller that named one it could not resolve. A value that is no WAFv2 web ACL ARN of the
/// global namespace — another resource kind, the regional namespace, a WAF Classic
/// identifier, a string that is no ARN at all — names no web ACL to resolve and so resolves
/// to nothing here too.
///
/// `false` as well when the WAFv2 store cannot be reached at all: a web ACL whose existence
/// nothing can confirm is not one the account previously validated.
///
/// Call it with NO lock of this service's store held: it locks the wafv2 store, and that
/// service holds no record of this one, so the two are never held at the same time.
pub fn associable<Eng: Send + Sync + 'static>(engine: &Eng, web_acl_arn: &str) -> bool {
    if !crate::state::associable_web_acl_arn(web_acl_arn) {
        return false;
    }
    let Some(waf) = ::service_com_amazonaws_wafv2_awswaf_20190729::state::service_state(engine)
    else {
        return false;
    };
    let store = waf.lock();
    store.web_acl_by_arn(web_acl_arn).is_some()
}

/// The sentence CloudFront refuses a CONFIGURATION whose `WebACLId` names a WAFv2 web ACL
/// the requester cannot reach with: the whole text is fixed — it says the web ACL named is
/// not one this requester can use, and names neither the identifier submitted nor the reason
/// it did not resolve — so nothing of the request reaches it. Recorded behaviour.
///
/// It is the OTHER sentence of this one resolution: [`crate::state::INVALID_WEB_ACL_MESSAGE`]
/// is what an ASSOCIATION call is told, whose `WebACLArn` is the whole request, and this one
/// is what a write carrying a whole `DistributionConfig` is told about one member of it.
pub const INACCESSIBLE_WEB_ACL_MESSAGE: &str = "Web ACL is not accessible by the requester.";

/// `Some(message)` when the `WebACLId` of `config` names a WAFv2 web ACL this account does
/// not hold for CloudFront, and `None` when it names one it does — or names no web ACL this
/// emulator can resolve at all.
///
/// A distribution reports the web ACL in front of it through this one member, so a
/// configuration naming one that resolves to nothing would report a protection that does not
/// exist. The web ACL is therefore a resource the write RESOLVES rather than a string it
/// stores, against the service that HOLDS web ACLs ([`associable`]) — which is the same
/// question, asked of the same records, that an association call resolves before it writes
/// the very same member.
///
/// Only a value that NAMES a WAFv2 resource is resolved: `WebACLId` carries either
/// generation — "to specify a web ACL created using the latest version of WAF, use the ACL
/// ARN … To specify a web ACL created using WAF Classic, use the ACL ID" — and a WAF Classic
/// identifier names a resource of a service this emulator holds no web ACL of, so it is
/// evidence of nothing and is stored as submitted. A configuration naming no web ACL at all
/// states the empty value, which names nothing to resolve.
///
/// Call it with NO lock of this service's store held, for the reason [`associable`] gives.
pub fn inaccessible_web_acl<Eng: Send + Sync + 'static>(
    engine: &Eng,
    config: &crate::ops::types::DistributionConfig,
) -> Option<&'static str> {
    let web_acl_id = config.web_acl_id.as_deref().unwrap_or_default().trim();
    if !names_a_wafv2_resource(web_acl_id) || associable(engine, web_acl_id) {
        return None;
    }
    Some(INACCESSIBLE_WEB_ACL_MESSAGE)
}

/// Whether `web_acl_id` names a WAFv2 resource at all, which is what makes it a value the
/// service that holds web ACLs can be asked about.
///
/// The ARN layout is read by the service that mints it
/// (`service_com_amazonaws_wafv2_awswaf_20190729::state::is_wafv2_resource_arn`), so this
/// service holds no second reading of a WAFv2 ARN.
fn names_a_wafv2_resource(web_acl_id: &str) -> bool {
    ::service_com_amazonaws_wafv2_awswaf_20190729::state::is_wafv2_resource_arn(web_acl_id)
}

/// Whether `web_acl_id` names a WAFv2 web ACL by its ARN.
///
/// `WebACLId` carries either form — "to specify a web ACL created using the latest version
/// of WAF, use the ACL ARN … to specify a web ACL created using WAF Classic, use the ACL
/// ID" — and the two are told apart by the shape the value takes: an ARN begins with the
/// `arn:` prefix every Amazon Resource Name does, and a WAF Classic id does not. A
/// configuration that names none states the empty string, which names nothing.
fn names_a_web_acl_arn(web_acl_id: &str) -> bool {
    web_acl_id.starts_with("arn:")
}
