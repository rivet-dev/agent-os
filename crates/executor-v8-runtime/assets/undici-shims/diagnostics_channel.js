"use strict";

const subscribers = new Map();
const channels = new Map();

function ensureSubscribers(name) {
	let list = subscribers.get(name);
	if (!list) {
		list = new Set();
		subscribers.set(name, list);
	}
	return list;
}

function createChannel(name) {
	return {
		name,
		get hasSubscribers() {
			return ensureSubscribers(name).size > 0;
		},
		publish(message) {
			for (const subscriber of ensureSubscribers(name)) {
				subscriber(message, name);
			}
		},
		subscribe(onMessage) {
			ensureSubscribers(name).add(onMessage);
			return this;
		},
		unsubscribe(onMessage) {
			ensureSubscribers(name).delete(onMessage);
			return this;
		},
	};
}

function channel(name) {
	if (!channels.has(name)) {
		channels.set(name, createChannel(name));
	}
	return channels.get(name);
}

function subscribe(name, onMessage) {
	channel(name).subscribe(onMessage);
}

function unsubscribe(name, onMessage) {
	channel(name).unsubscribe(onMessage);
}

module.exports = {
	channel,
	hasSubscribers(name) {
		return channel(name).hasSubscribers;
	},
	subscribe,
	unsubscribe,
};
