// Checks the licenses of every installed package (ADR-0058): the web
// experience takes only permissively licensed code (MIT, Apache-2.0, BSD).
// The build tools are development dependencies, never shipped; what ships
// is the bundle Vite builds from Svelte and SvelteKit's runtime.

import { readdirSync, readFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';

// ISC is the BSD family's simplest form (OpenBSD's license).
const allowed = new Set(['MIT', 'Apache-2.0', 'BSD-2-Clause', 'BSD-3-Clause', '0BSD', 'ISC']);

interface Manifest {
	name?: unknown;
	version?: unknown;
	license?: unknown;
}

function licenseOf(manifest: Manifest): string {
	return typeof manifest.license === 'string' ? manifest.license : 'UNKNOWN';
}

// An SPDX expression is acceptable if one side of every OR is allowed and
// every side of an AND is.
function acceptable(expression: string): boolean {
	const text = expression.replace(/[()]/g, ' ').trim();
	return text
		.split(/\s+OR\s+/)
		.some((choice) => choice.split(/\s+AND\s+/).every((part) => allowed.has(part.trim())));
}

function* packages(dir: string): Generator<string> {
	if (!existsSync(dir)) return;
	for (const entry of readdirSync(dir, { withFileTypes: true })) {
		if (!entry.isDirectory() || entry.name.startsWith('.')) continue;
		const path = join(dir, entry.name);
		if (entry.name.startsWith('@')) {
			yield* packages(path);
			continue;
		}
		if (existsSync(join(path, 'package.json'))) yield path;
		yield* packages(join(path, 'node_modules'));
	}
}

const found = new Map<string, string>();
const refused: string[] = [];
for (const path of packages('node_modules')) {
	const manifest = JSON.parse(readFileSync(join(path, 'package.json'), 'utf8')) as Manifest;
	const name = `${String(manifest.name)}@${String(manifest.version)}`;
	const license = licenseOf(manifest);
	found.set(name, license);
	if (!acceptable(license)) refused.push(`${name}: ${license}`);
}

const counts = new Map<string, number>();
for (const license of found.values()) counts.set(license, (counts.get(license) ?? 0) + 1);
console.log(
	`${found.size} packages: ` +
		[...counts].map(([license, n]) => `${license} ${n}`).join(', ')
);
if (refused.length > 0) {
	console.error(`not allowed (MIT, Apache-2.0 and BSD only):\n  ${refused.join('\n  ')}`);
	process.exit(1);
}
