// Refreshes on a timer while the page is visible: the bridge serves one
// request at a time, so a hidden tab should not keep it busy.

export interface Visibility {
	hidden: boolean;
	addEventListener(type: 'visibilitychange', listener: () => void): void;
	removeEventListener(type: 'visibilitychange', listener: () => void): void;
}

export interface Timers {
	setTimeout(callback: () => void, ms: number): unknown;
	clearTimeout(handle: unknown): void;
}

/**
 * Calls `refresh` every `ms` while visible (never two at once), and once
 * when the page becomes visible again. Returns the function that stops it.
 */
export function poll(
	refresh: () => Promise<unknown>,
	ms: number,
	visibility: Visibility = document,
	timers: Timers = {
		setTimeout: (callback, delay) => globalThis.setTimeout(callback, delay),
		clearTimeout: (handle) => globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>)
	}
): () => void {
	let timer: unknown = null;
	let stopped = false;
	let running = false;

	const schedule = () => {
		if (!stopped && !visibility.hidden && timer === null) {
			timer = timers.setTimeout(tick, ms);
		}
	};

	async function tick() {
		timer = null;
		if (stopped || running || visibility.hidden) return;
		running = true;
		try {
			await refresh();
		} catch {
			// The page shows its own errors; keep polling.
		} finally {
			running = false;
			schedule();
		}
	}

	const onVisibility = () => {
		if (!visibility.hidden && timer === null && !running) void tick();
	};

	visibility.addEventListener('visibilitychange', onVisibility);
	schedule();
	return () => {
		stopped = true;
		if (timer !== null) timers.clearTimeout(timer);
		visibility.removeEventListener('visibilitychange', onVisibility);
	};
}
