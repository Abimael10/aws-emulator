// SPDX-License-Identifier: CC0-1.0

//! The absence of the template DATA a send renders with, as each sending surface words it,
//! end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this service
//! registered — through the shared harness in the sibling [`crate::configuration_sets`]
//! suite, so routing, deserialization, the handler and the error envelope are all exercised.
//!
//! RULE: a templated send states the values its message is rendered with, and a request that
//! carries no such member has not stated them — a failure of the REQUEST, settled before
//! anything the ACCOUNT holds is resolved (its sending identities, its templates), and worded
//! as THAT surface words it: `SendEmail` reports `Template data cannot be empty` for the
//! `Content.Template` it renders one message from, and `SendBulkEmail` reports `The default
//! template data does not exist` for the `DefaultContent.Template` every destination's
//! message is rendered from. One absence, two surfaces, two recorded sentences — so a client
//! reading one is never handed the other's.
//!
//! The rows differ along what the rule generalises over: the surface, whether the sending
//! address is one a verified identity of the account authorises, and whether the content
//! names a template the account holds — none of which changes the refusal, because what the
//! request has not stated is settled ahead of all of them.
//!
//! What the member is refused for when it IS stated but is no set of key-value pairs, and the
//! whole table of forms it can take, is the sibling [`crate::bulk_default_template_data`]
//! suite's rule.

use serde_json::{json, Value};

use crate::configuration_sets::{Reply, Ses};

/// The routes these cases drive, as the model binds them
/// (`smithy_query(query="operation",operation="SendEmail,SendBulkEmail")`).
const OUTBOUND_EMAILS: &str = "/v2/email/outbound-emails";
const OUTBOUND_BULK_EMAILS: &str = "/v2/email/outbound-bulk-emails";
const TEMPLATES: &str = "/v2/email/templates";

/// The domain identity the account holds, and the address inside it a verified row sends
/// from.
const SENDER_DOMAIN: &str = "senders.example.com";
const SENDER: &str = "sender@senders.example.com";

/// An address no identity of this account authorises, for the rows that show the absence is
/// settled before the sending identity is weighed.
const STRANGER: &str = "stranger@example.com";

/// The stored template the rows render through, and its parts.
const TEMPLATE: &str = "receipt";
const TEMPLATE_SUBJECT: &str = "Your receipt, {{name}}";
const TEMPLATE_TEXT: &str = "Thanks {{name}}.";

/// AWS's sentence for a SINGLE send that states no values at all: wholly fixed text.
const NO_DATA_ON_SEND: &str = "Template data cannot be empty";

/// AWS's sentence for a BULK send that states no default values at all: wholly fixed text,
/// and not the one above.
const NO_DEFAULT_DATA_ON_BULK: &str = "The default template data does not exist";

/// The calls these cases drive beside the shared harness's own.
trait TemplateDataCalls {
    async fn store_template(&self) -> Reply;
    async fn send(&self, body: Value) -> Reply;
    async fn send_bulk(&self, body: Value) -> Reply;
    async fn sent_last_24_hours(&self) -> f64;
}

impl TemplateDataCalls for Ses {
    async fn store_template(&self) -> Reply {
        self.call(
            "POST",
            TEMPLATES,
            Some(json!({
                "TemplateName": TEMPLATE,
                "TemplateContent": {"Subject": TEMPLATE_SUBJECT, "Text": TEMPLATE_TEXT},
            })),
        )
        .await
    }

    async fn send(&self, body: Value) -> Reply {
        self.call("POST", OUTBOUND_EMAILS, Some(body)).await
    }

    async fn send_bulk(&self, body: Value) -> Reply {
        self.call("POST", OUTBOUND_BULK_EMAILS, Some(body)).await
    }

    /// What `GetAccount` reports of the sending quota this window has charged, which a
    /// refused call must leave where it was.
    async fn sent_last_24_hours(&self) -> f64 {
        let account = self.call("GET", "/v2/email/account", None).await;
        account
            .pointer("/SendQuota/SentLast24Hours")
            .and_then(|value| value.as_f64())
            .unwrap_or_default()
    }
}

/// A service holding the stored template, and the sending domain when `verified`.
async fn seeded(verified: bool) -> Ses {
    let ses = Ses::new();
    let stored = ses.store_template().await;
    assert_eq!(
        stored.status,
        200,
        "the template must be stored first, got {} {:?}",
        stored.status,
        stored.message()
    );
    if verified {
        ses.verify_identity(SENDER_DOMAIN).await;
    }
    ses
}

/// Which surface a row sends through: the two that render a template from data the request
/// states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    /// `SendEmail`, whose `Content.Template` renders the one message.
    Single,
    /// `SendBulkEmail`, whose `DefaultContent.Template` renders every destination's.
    Bulk,
}

impl Surface {
    /// The sentence this surface words the absence with.
    fn sentence(self) -> &'static str {
        match self {
            Surface::Single => NO_DATA_ON_SEND,
            Surface::Bulk => NO_DEFAULT_DATA_ON_BULK,
        }
    }

    /// One request through this surface, from `from`, naming `template_name` and carrying NO
    /// `TemplateData` member at all.
    fn request(self, from: &str, template_name: &str) -> Value {
        let template = json!({"TemplateName": template_name});
        match self {
            Surface::Single => json!({
                "FromEmailAddress": from,
                "Destination": {"ToAddresses": ["reader@example.net"]},
                "Content": {"Template": template},
            }),
            Surface::Bulk => json!({
                "FromEmailAddress": from,
                "DefaultContent": {"Template": template},
                "BulkEmailEntries": [
                    {"Destination": {"ToAddresses": ["reader@example.net"]}},
                    {"Destination": {"ToAddresses": ["other@example.net"]}},
                ],
            }),
        }
    }

    /// The request sent through this surface.
    async fn sent(self, ses: &Ses, body: Value) -> Reply {
        match self {
            Surface::Single => ses.send(body).await,
            Surface::Bulk => ses.send_bulk(body).await,
        }
    }
}

#[tokio::test]
async fn a_templated_send_that_states_no_template_data_is_refused_in_that_surfaces_words() {
    // The rows span the surface, the state of the sending address and whether the named
    // template is one the account holds: the refusal is the surface's own sentence in every
    // combination, because the member the request left out is settled before the identity
    // gate and before any template is resolved.
    for surface in [Surface::Single, Surface::Bulk] {
        for verified in [false, true] {
            for (named, template_name) in [
                ("a template the account holds", TEMPLATE),
                (
                    "a template name the account holds nothing under",
                    "no-such-template",
                ),
            ] {
                let case = format!("{surface:?} from a verified sender: {verified}, {named}");
                let ses = seeded(verified).await;
                let from = if verified { SENDER } else { STRANGER };
                let reply = surface
                    .sent(&ses, surface.request(from, template_name))
                    .await;
                assert_eq!(
                    (reply.status, reply.error_code().as_str()),
                    (400, "BadRequestException"),
                    "case {case:?}: expected 400 BadRequestException, got {} {:?}",
                    reply.status,
                    reply.error_code()
                );
                assert_eq!(
                    reply.message(),
                    surface.sentence(),
                    "case {case:?}: expected the message {:?}, got {:?}",
                    surface.sentence(),
                    reply.message()
                );
                // ...and the two sentences are distinct, so neither surface can be read as
                // the other.
                assert_ne!(
                    reply.message(),
                    match surface {
                        Surface::Single => NO_DEFAULT_DATA_ON_BULK,
                        Surface::Bulk => NO_DATA_ON_SEND,
                    },
                    "case {case:?}: expected this surface's own sentence"
                );
                // The refused request sent nothing: nothing is charged against the quota.
                let charged = ses.sent_last_24_hours().await;
                assert_eq!(
                    charged, 0.0,
                    "case {case:?}: a refused send must charge nothing, got {charged}"
                );
            }
        }
    }
}
