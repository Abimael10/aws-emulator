// SPDX-License-Identifier: CC0-1.0

//! The live-subscription bookkeeping of one enhanced fan-out consumer: which shards it is
//! reading right now, and which of those reads a later subscription of the same consumer to
//! the same shard has CONTESTED.
//!
//! "If you call SubscribeToShard again with the same ConsumerARN and ShardId within 5 seconds
//! of a successful call, you'll get a ResourceInUseException. If you call SubscribeToShard 5
//! seconds or more after a successful call, the second call takes over the subscription"
//! (`SubscribeToShard`): a subscription is contended for exactly
//! [`SUBSCRIPTION_TAKEOVER_WINDOW`] after it was established, and what contention produces is
//! an IN-BAND event of the `SubscribeToShardEventStream` union — the `ResourceInUseException`
//! member — on BOTH event streams, not a refusal of the second call, which the service answers
//! with a live stream of its own.

use super::*;

/// ONE subscription of a consumer to a shard whose event stream is still live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveSubscription {
    /// When the subscription was established, which is what the contention window above is
    /// counted from and what tells two subscriptions of one consumer to one shard apart.
    pub established_at: SystemTime,
    /// Whether a LATER subscription of the same consumer to the same shard was established
    /// while this one was still live: the two are contending for the shard, so this one is
    /// faulted in band rather than going on reading.
    pub contested: bool,
}

impl LiveSubscription {
    /// The subscription a call at `at` established: it holds its shard and nothing contends
    /// with it yet.
    pub fn established_at(at: SystemTime) -> Self {
        Self {
            established_at: at,
            contested: false,
        }
    }

    /// Whether this subscription is still one a later call contends with at `now`: the window
    /// above bounds how long that is. A subscription established at an instant later than
    /// `now` — a wall clock that moved backwards — still holds its shard.
    fn live_at(&self, now: SystemTime) -> bool {
        !now.duration_since(self.established_at)
            .is_ok_and(|since| since >= SUBSCRIPTION_TAKEOVER_WINDOW)
    }
}

impl ConsumerRecord {
    /// Whether a subscription of this consumer to `shard_id` is still LIVE at `now`, so a
    /// second one contends with it rather than taking it over. Only a subscription that has
    /// not ended is recorded at all, and the window above bounds even that one, so a renewal
    /// of a subscription whose event stream is over contends with nothing. Decided from the
    /// stored instant, so two reads at the same instant agree.
    pub fn holds_subscription(&self, shard_id: &str, now: SystemTime) -> bool {
        self.subscriptions
            .get(shard_id)
            .is_some_and(|live| live.live_at(now))
    }

    /// Record that a subscription of this consumer to `shard_id` was established at `at` and
    /// is now live, which is what the window above is then counted from. The entries the
    /// window has rolled past are dropped with it, so the record holds at most the
    /// subscriptions that are live.
    pub fn subscribed_at(&mut self, shard_id: &str, at: SystemTime) {
        self.subscriptions.retain(|_, live| live.live_at(at));
        self.subscriptions
            .insert(shard_id.to_string(), LiveSubscription::established_at(at));
    }

    /// Mark the live subscription of this consumer to `shard_id` as CONTESTED by a second
    /// subscription established at `now`, answering whether there was one to contest.
    ///
    /// The shard goes on being held by the subscription that was already reading it — the
    /// contesting call establishes nothing of its own, because both event streams are about
    /// to be faulted with the union's `ResourceInUseException` member and the shard is free
    /// again once the incumbent's stream is over.
    pub fn contend_subscription(&mut self, shard_id: &str, now: SystemTime) -> bool {
        match self.subscriptions.get_mut(shard_id) {
            Some(live) if live.live_at(now) => {
                live.contested = true;
                true
            }
            _ => false,
        }
    }

    /// Whether the subscription of this consumer to `shard_id` that was established at
    /// `established_at` has been contested by a later one, which is what its own event stream
    /// reports in band before it ends.
    ///
    /// Asked of the subscription BY the instant it was established, so a subscription that is
    /// over, or one another call established afterwards, is never mistaken for this one.
    pub fn subscription_contested(&self, shard_id: &str, established_at: SystemTime) -> bool {
        self.subscriptions
            .get(shard_id)
            .is_some_and(|live| live.established_at == established_at && live.contested)
    }

    /// Release the shard a subscription of this consumer to `shard_id` held: its event
    /// stream has ENDED, so the subscription is no longer one a later call contends with.
    /// "Your consumer starts receiving events of type SubscribeToShardEvent over the HTTP/2
    /// connection for up to 5 minutes, after which time you need to call SubscribeToShard
    /// again to renew the subscription if you want to continue to receive records"
    /// (`SubscribeToShard`) — that renewal is what this makes possible, because a stream
    /// that is over holds nothing.
    pub fn subscription_ended(&mut self, shard_id: &str) {
        self.subscriptions.remove(shard_id);
    }

    /// Release the shard ONE subscription held: the one established at `established_at`, and
    /// nothing else.
    ///
    /// The same end as [`Self::subscription_ended`] for the caller that established it, and a
    /// no-op for a caller whose subscription a LATER one has already taken over ("if you call
    /// SubscribeToShard 5 seconds or more after a successful call, the second call takes over
    /// the subscription") — so an ending stream cannot release a shard the subscription now
    /// reading it holds.
    pub fn subscription_ended_at(&mut self, shard_id: &str, established_at: SystemTime) {
        if self
            .subscriptions
            .get(shard_id)
            .is_some_and(|live| live.established_at == established_at)
        {
            self.subscriptions.remove(shard_id);
        }
    }
}

/// How a subscription states, IN BAND, that another one of the same consumer is already
/// reading the shard it asked for: "If you call SubscribeToShard again with the same
/// ConsumerARN and ShardId within 5 seconds of a successful call, you'll get a
/// ResourceInUseException" (`SubscribeToShard`), which travels as the `ResourceInUseException`
/// member of the `SubscribeToShardEventStream` union. Built FROM the request and the stored
/// records, so the account, the stream, the shard and the consumer that contend name it.
pub fn another_active_subscription_message(
    stream_name: &str,
    shard_id: &str,
    consumer_name: &str,
) -> String {
    format!(
        "Another active subscription exists for this consumer: {}:{stream_name}:{shard_id}:{consumer_name}",
        ::engine::emulator_account_id()
    )
}
