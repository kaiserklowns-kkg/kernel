// The typed client of the Oceans System API, as the bridge serves it
// (go/cmd/bridge, ADR-0058). Every response is checked against the type
// it claims before the app sees it: the client trusts the shape of
// nothing it did not check.

export interface Session {
	paired: boolean;
	authenticated: boolean;
}

export interface Kernel {
	version: string;
	arch: string;
	abi: number;
}

export interface Memory {
	totalMiB: number;
	freeMiB: number;
	kernelHeapKiB: number;
}

export interface Process {
	id: number;
	parent: number;
	name: string;
	memoryKiB: number;
}

export interface SystemInfo {
	kernel: Kernel;
	memory: Memory;
	uptimeSeconds: number;
	processes: Process[];
}

export interface App {
	id: string;
	version: string;
	name: string;
	running: boolean;
	/** `app`, or `service` with whether it is enabled. */
	kind: string;
	/** `native`, or `wasm` for a Go app run by the Go host. */
	runtime: string;
	publisher: string;
}

export const decisions = ['automatic', 'allowed', 'denied', 'undecided'] as const;
export type Decision = (typeof decisions)[number];

export interface Permission {
	name: string;
	/** What it means, in the system's words. */
	description: string;
	decision: Decision;
	/** The app's declared reason, in its own words. */
	reason: string;
}

export const aiStates = ['done', 'needs-approval', 'failed'] as const;
export type AIState = (typeof aiStates)[number];

export interface AISession {
	session: number;
	state: AIState;
	/** The answer, why it failed, or the approval question (worded by the system). */
	text: string;
}

export interface ModelSetting {
	url: string;
	model: string;
	note: string;
}

export const storeStates = ['available', 'installed', 'update'] as const;
export type StoreState = (typeof storeStates)[number];

/** An app a store lists (ADR-0061), and its state on this system. */
export interface StoreApp {
	id: string;
	name: string;
	version: string;
	publisher: string;
	description: string;
	permissions: string[];
	size: number;
	state: StoreState;
}

export interface StoreView {
	/** The store's URL; empty if none is set. */
	source: string;
	apps: StoreApp[];
}

/** A failed request: the HTTP status and the system's explanation. */
export class ApiError extends Error {
	readonly status: number;

	constructor(status: number, message: string) {
		super(message);
		this.name = 'ApiError';
		this.status = status;
	}

	/** The pairing is missing, wrong or was revoked (`ui unpair`). */
	get unauthorized(): boolean {
		return this.status === 401;
	}
}

// ---- Checking what arrives ------------------------------------------------

type Check<T> = (value: unknown, where: string) => T;

function fail(where: string, expected: string): never {
	throw new ApiError(0, `unexpected response: ${where} is not ${expected}`);
}

function record(value: unknown, where: string): Record<string, unknown> {
	if (typeof value !== 'object' || value === null || Array.isArray(value)) fail(where, 'an object');
	return value as Record<string, unknown>;
}

const string: Check<string> = (value, where) =>
	typeof value === 'string' ? value : fail(where, 'a string');

const number: Check<number> = (value, where) =>
	typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : fail(where, 'a number');

const boolean: Check<boolean> = (value, where) =>
	typeof value === 'boolean' ? value : fail(where, 'true or false');

function oneOf<T extends string>(choices: readonly T[]): Check<T> {
	return (value, where) =>
		typeof value === 'string' && (choices as readonly string[]).includes(value)
			? (value as T)
			: fail(where, `one of ${choices.join(', ')}`);
}

function list<T>(item: Check<T>): Check<T[]> {
	return (value, where) => {
		if (!Array.isArray(value)) fail(where, 'a list');
		return value.map((element, i) => item(element, `${where}[${i}]`));
	};
}

type Shape<T> = { [K in keyof T]: Check<T[K]> };

function object<T>(shape: Shape<T>): Check<T> {
	return (value, where) => {
		const fields = record(value, where);
		const out: Partial<T> = {};
		for (const key of Object.keys(shape) as (keyof T & string)[]) {
			out[key] = shape[key](fields[key], `${where}.${key}`);
		}
		return out as T;
	};
}

export const checks = {
	session: object<Session>({ paired: boolean, authenticated: boolean }),
	system: object<SystemInfo>({
		kernel: object<Kernel>({ version: string, arch: string, abi: number }),
		memory: object<Memory>({ totalMiB: number, freeMiB: number, kernelHeapKiB: number }),
		uptimeSeconds: number,
		processes: list(object<Process>({ id: number, parent: number, name: string, memoryKiB: number }))
	}),
	apps: list(
		object<App>({
			id: string,
			version: string,
			name: string,
			running: boolean,
			kind: string,
			runtime: string,
			publisher: string
		})
	),
	permissions: list(
		object<Permission>({ name: string, description: string, decision: oneOf(decisions), reason: string })
	),
	lines: list(string),
	ai: object<AISession>({ session: number, state: oneOf(aiStates), text: string }),
	model: object<ModelSetting>({ url: string, model: string, note: string }),
	changed: object<{ id: string; state: string }>({ id: string, state: string }),
	store: object<StoreView>({
		source: string,
		apps: list(
			object<StoreApp>({
				id: string,
				name: string,
				version: string,
				publisher: string,
				description: string,
				permissions: (value, where) => (value === null || value === undefined ? [] : list(string)(value, where)),
				size: number,
				state: oneOf(storeStates)
			})
		)
	}),
	source: object<{ source: string }>({ source: string })
};

// ---- The client -----------------------------------------------------------

export type Fetch = (input: string, init: RequestInit) => Promise<Response>;

/** The longest question the AI runtime takes (one IPC message). */
export const maxQuestion = 240;

const appId = /^[a-z0-9._-]{1,64}$/;

function appPath(id: string, action: string): string {
	if (!appId.test(id)) throw new ApiError(0, `not an app id: ${id}`);
	return `/api/apps/${id}/${action}`;
}

export function createClient(fetchImpl: Fetch, base = '') {
	async function request<T>(method: string, path: string, check: Check<T> | null, body?: unknown): Promise<T> {
		const headers: Record<string, string> = { Accept: 'application/json' };
		const init: RequestInit = { method, headers, credentials: 'same-origin', cache: 'no-store' };
		if (body !== undefined) {
			headers['Content-Type'] = 'application/json';
			init.body = JSON.stringify(body);
		}
		let response: Response;
		try {
			response = await fetchImpl(base + path, init);
		} catch {
			throw new ApiError(0, 'Oceans did not answer: is it running, and on this network?');
		}
		const text = await response.text();
		let value: unknown = undefined;
		if (text !== '') {
			try {
				value = JSON.parse(text);
			} catch {
				throw new ApiError(response.status, `unexpected response (${response.status})`);
			}
		}
		if (!response.ok) {
			const message =
				typeof value === 'object' && value !== null && typeof (value as { error?: unknown }).error === 'string'
					? (value as { error: string }).error
					: `request failed (${response.status})`;
			throw new ApiError(response.status, message);
		}
		return check === null ? (undefined as T) : check(value, 'response');
	}

	return {
		session: () => request('GET', '/api/session', checks.session),
		/** Exchanges the pairing code for this browser's HttpOnly session cookie. */
		login: (token: string) => request('POST', '/api/session', null, { token: token.trim() }),
		logout: () => request('DELETE', '/api/session', null),
		system: () => request('GET', '/api/system', checks.system),
		apps: () => request('GET', '/api/apps', checks.apps),
		start: async (id: string, args = '') => request('POST', appPath(id, 'start'), checks.changed, { args }),
		stop: async (id: string) => request('POST', appPath(id, 'stop'), checks.changed, {}),
		permissions: async (id: string) => request('GET', appPath(id, 'permissions'), checks.permissions),
		audit: () => request('GET', '/api/audit', checks.lines),
		ask: (question: string) => {
			const q = question.trim();
			if (q === '' || new TextEncoder().encode(q).length > maxQuestion) {
				return Promise.reject(new ApiError(0, `ask one line of at most ${maxQuestion} bytes`));
			}
			return request('POST', '/api/ai/ask', checks.ai, { question: q });
		},
		/** The user's answer to an approval question: only an explicit click sends it. */
		answer: (session: number, approve: boolean) =>
			request('POST', '/api/ai/continue', checks.ai, { session, approve }),
		activity: () => request('GET', '/api/ai/activity', checks.lines),
		setModel: (url: string, model: string) =>
			request('POST', '/api/ai/model', checks.model, { url: url.trim(), model: model.trim() }),
		store: () => request('GET', '/api/store', checks.store),
		setStoreSource: (url: string) => request('POST', '/api/store/source', checks.source, { url: url.trim() }),
		/** Proposes the install: Oceans asks the user on the device before anything is installed. */
		install: async (id: string) => {
			if (!appId.test(id)) throw new ApiError(0, `not an app id: ${id}`);
			return request('POST', '/api/store/install', checks.changed, { id });
		}
	};
}

export type Client = ReturnType<typeof createClient>;

/** The client the app uses: same origin, with the browser's cookie. */
export const api: Client = createClient((input, init) => fetch(input, init));
