import { describe, expect, test } from 'bun:test';
import { ApiError, createClient, type Fetch } from './api';

interface Seen {
	url: string;
	method: string;
	headers: Record<string, string>;
	body: unknown;
}

/** A fetch answering `status` with `body`, recording what was sent. */
function fake(status: number, body: unknown): { fetch: Fetch; seen: Seen[] } {
	const seen: Seen[] = [];
	const fetch: Fetch = async (url, init) => {
		seen.push({
			url,
			method: init.method ?? 'GET',
			headers: { ...(init.headers as Record<string, string>) },
			body: typeof init.body === 'string' ? JSON.parse(init.body) : undefined
		});
		const text = body === undefined ? '' : typeof body === 'string' ? body : JSON.stringify(body);
		return new Response(text, { status });
	};
	return { fetch, seen };
}

async function failure(promise: Promise<unknown>): Promise<ApiError> {
	try {
		await promise;
	} catch (error) {
		if (error instanceof ApiError) return error;
		throw error;
	}
	throw new Error('the request succeeded');
}

const system = {
	kernel: { version: '0.1.0', arch: 'x86_64', abi: 9 },
	memory: { totalMiB: 240, freeMiB: 200, kernelHeapKiB: 512 },
	uptimeSeconds: 12,
	processes: [{ id: 1, parent: 0, name: 'init', memoryKiB: 64 }]
};

describe('the client', () => {
	test('reads the system information', async () => {
		const { fetch, seen } = fake(200, system);
		const info = await createClient(fetch).system();
		expect(info.memory.totalMiB).toBe(240);
		expect(info.processes[0]?.name).toBe('init');
		expect(seen[0]).toMatchObject({ url: '/api/system', method: 'GET' });
		expect(seen[0]?.headers['Content-Type']).toBeUndefined();
	});

	test('sends JSON for changes', async () => {
		const { fetch, seen } = fake(200, { id: 'app.oceans.hello', state: 'running' });
		await createClient(fetch, 'http://oceans').start('app.oceans.hello', 'wait');
		expect(seen[0]).toMatchObject({
			url: 'http://oceans/api/apps/app.oceans.hello/start',
			method: 'POST',
			body: { args: 'wait' }
		});
		expect(seen[0]?.headers['Content-Type']).toBe('application/json');
	});

	test('refuses ids that are not app ids before sending', async () => {
		const { fetch, seen } = fake(200, {});
		const error = await failure(createClient(fetch).stop('../session'));
		expect(error.message).toContain('not an app id');
		expect(seen).toHaveLength(0);
	});

	test('reports the system’s explanation', async () => {
		const { fetch } = fake(409, { error: 'the app is already running' });
		const error = await failure(createClient(fetch).start('app.oceans.hello'));
		expect(error.status).toBe(409);
		expect(error.message).toBe('the app is already running');
		expect(error.unauthorized).toBe(false);
	});

	test('knows an unpaired browser', async () => {
		const { fetch } = fake(401, { error: 'not paired' });
		const error = await failure(createClient(fetch).apps());
		expect(error.unauthorized).toBe(true);
	});

	test('checks the shape of what arrives', async () => {
		for (const bad of [
			{ ...system, memory: { totalMiB: '240', freeMiB: 1, kernelHeapKiB: 1 } },
			{ ...system, processes: {} },
			{ ...system, uptimeSeconds: -1 },
			[],
			null
		]) {
			const { fetch } = fake(200, bad);
			const error = await failure(createClient(fetch).system());
			expect(error.message).toStartWith('unexpected response');
		}
		const { fetch } = fake(200, 'not json');
		expect((await failure(createClient(fetch).system())).message).toStartWith('unexpected response');
	});

	test('AI sessions and approvals', async () => {
		const asked = fake(200, { session: 3, state: 'needs-approval', text: 'start the app Hello (app.oceans.hello)' });
		const session = await createClient(asked.fetch).ask('  start the hello app ');
		expect(session.state).toBe('needs-approval');
		expect(asked.seen[0]?.body).toEqual({ question: 'start the hello app' });

		const answered = fake(200, { session: 3, state: 'done', text: 'Done' });
		await createClient(answered.fetch).answer(3, false);
		expect(answered.seen[0]?.body).toEqual({ session: 3, approve: false });

		const strange = fake(200, { session: 3, state: 'maybe', text: '' });
		expect((await failure(createClient(strange.fetch).answer(3, true))).message).toContain('one of');
	});

	test('bounds the question before sending', async () => {
		const { fetch, seen } = fake(200, {});
		await failure(createClient(fetch).ask('   '));
		await failure(createClient(fetch).ask('é'.repeat(121)));
		expect(seen).toHaveLength(0);
	});

	test('login and logout carry no answer', async () => {
		const { fetch, seen } = fake(204, undefined);
		const client = createClient(fetch);
		await client.login(' 0123456789abcdef0123456789abcdef ');
		await client.logout();
		expect(seen.map((s) => s.method)).toEqual(['POST', 'DELETE']);
		expect(seen[0]?.body).toEqual({ token: '0123456789abcdef0123456789abcdef' });
	});

	test('a network failure says so', async () => {
		const client = createClient(() => Promise.reject(new TypeError('offline')));
		const error = await failure(client.session());
		expect(error.status).toBe(0);
		expect(error.message).toContain('did not answer');
	});
});

describe('the Store', () => {
	const view = {
		source: 'http://10.0.2.2:8000/store',
		apps: [
			{
				id: 'app.oceans.tiles',
				name: 'Tiles',
				version: '1.0.0',
				publisher: 'Oceans Examples',
				description: 'A colour',
				permissions: ['window'],
				package: 'tiles-1.0.0.opk',
				size: 2048,
				sha256: '00',
				state: 'available'
			}
		]
	};

	test('reads the catalog with each app state', async () => {
		const { fetch } = fake(200, view);
		const store = await createClient(fetch).store();
		expect(store.apps[0]?.state).toBe('available');
		expect(store.apps[0]?.permissions).toEqual(['window']);
	});

	test('refuses an unknown state', async () => {
		const { fetch } = fake(200, { ...view, apps: [{ ...view.apps[0], state: 'hacked' }] });
		expect((await failure(createClient(fetch).store())).message).toContain('state');
	});

	test('proposes installs by id only', async () => {
		const { fetch, seen } = fake(202, { id: 'app.oceans.tiles', state: 'confirm on the device' });
		await createClient(fetch).install('app.oceans.tiles');
		expect(seen[0]).toMatchObject({ url: '/api/store/install', method: 'POST', body: { id: 'app.oceans.tiles' } });
		expect((await failure(createClient(fetch).install('../x'))).message).toContain('not an app id');
	});
});
