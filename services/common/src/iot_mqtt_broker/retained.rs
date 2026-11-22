// SPDX-License-Identifier: CC0-1.0

// Intentionally empty, and declared by nothing: the retained-message store this file held
// landed on mainline as part of `iot_mqtt_broker::MqttBrokerState` itself (`apply_retain`,
// `retained_messages`, `retained_topics`), so there is nothing left for a module of its own
// to own. The file is kept only because this fix could not remove it from the tree.
