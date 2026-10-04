import { expect, test } from 'bun:test';
import { poll, type Timers, type Visibility } from './poll';

function fakeTimers() {
	const pending = new Map<number, () => void>();
	let next = 1;
	const timers: Timers = {
		setTimeout(callback) {
			pending.set(next, callback);
			return next++;
		},
		clearTimeout(handle) {
			pending.delete(handle as number);
		}
	};
	const fire = async () => {
		const callbacks = [...pending.values()];
		pending.clear();
		for (const callback of callbacks) callback();
		await Promise.resolve();
		await Promise.resolve();
	};
	return { timers, pending, fire };
}

function fakePage(): Visibility & { change(hidden: boolean): void } {
	let listener: (() => void) | null = null;
	return {
		hidden: false,
		addEventListener: (_, l) => (listener = l),
		removeEventListener: () => (listener = null),
		change(hidden) {
			this.hidden = hidden;
			listener?.();
		}
	};
}

test('refreshes while visible, pauses while hidden, resumes at once', async () => {
	const { timers, pending, fire } = fakeTimers();
	const page = fakePage();
	let calls = 0;
	const stop = poll(async () => calls++, 1000, page, timers);
	expect(pending.size).toBe(1);
	await fire();
	expect(calls).toBe(1);
	expect(pending.size).toBe(1);

	page.change(true);
	await fire();
	expect(calls).toBe(1);
	expect(pending.size).toBe(0);

	page.change(false);
	await Promise.resolve();
	expect(calls).toBe(2);

	stop();
	await fire();
	expect(calls).toBe(2);
	expect(pending.size).toBe(0);
});

test('keeps polling after a failed refresh', async () => {
	const { timers, pending, fire } = fakeTimers();
	let calls = 0;
	poll(
		async () => {
			calls++;
			throw new Error('down');
		},
		1000,
		fakePage(),
		timers
	);
	await fire();
	await fire();
	expect(calls).toBe(2);
	expect(pending.size).toBe(1);
});
