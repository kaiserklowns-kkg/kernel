// The app API of Oceans web apps (ADR-0064): the app's own data, as text
// under short names (a-z 0-9 . _ -, at most 16 KiB each). The bridge puts
// this page's token in it; every request presents it. The app's manifest
// must ask for `storage`.
import { base } from '$app/paths';

function token() {
	return document.querySelector('meta[name="oceans-app-token"]')?.getAttribute('content') ?? '';
}

function url(name) {
	if (!/^[a-z0-9._-]{1,64}$/.test(name) || name.startsWith('.')) throw new Error(`not a data name: ${name}`);
	return `${base}/api/data/${name}`;
}

/** The text stored under `name`, or null if there is none. */
export async function load(name) {
	const response = await fetch(url(name), { headers: { Authorization: `Bearer ${token()}` } });
	if (response.status === 404) return null;
	if (!response.ok) throw new Error(`loading ${name}: ${response.status}`);
	return response.text();
}

/** Stores `text` under `name`. */
export async function save(name, text) {
	const response = await fetch(url(name), {
		method: 'PUT',
		headers: { Authorization: `Bearer ${token()}`, 'Content-Type': 'text/plain' },
		body: text
	});
	if (!response.ok) throw new Error(`saving ${name}: ${response.status}`);
}
