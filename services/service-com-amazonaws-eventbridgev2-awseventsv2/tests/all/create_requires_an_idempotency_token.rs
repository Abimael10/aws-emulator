// SPDX-License-Identifier: CC0-1.0

//! Retired: the rule this suite used to pin — that every create of this service refuses a
//! request stating no `ClientToken` — is not this service's.
//!
//! The model marks the member `@idempotencyToken` and not `@required` on each of the three
//! creates
//! (`smithy_query(query="operation",operation="CreateEventBus,CreateEventSource,CreateSubscriber")`),
//! so a create that omits it is SERVED and mints its resource; the token is read only to
//! replay a retry that states one. `crate::create_without_an_idempotency_token` pins that,
//! over the same three creates and the fan-out a minted bus exists for, and carries on the
//! two cases of this suite that were never about the refusal: the operations that model no
//! token at all served without one, and a tokenless create naming a bus that does not exist
//! refused for that bus. `crate::modelled_input_refusal` keeps the refusals the model really
//! does declare.
