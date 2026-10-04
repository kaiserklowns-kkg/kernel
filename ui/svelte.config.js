import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

// The one inline style the app has: SvelteKit's route announcer (for
// screen readers) carries a fixed style attribute. The policy allows
// exactly that attribute by its hash ('unsafe-hashes' covers attributes,
// and only those whose hash is listed), read from the installed SvelteKit
// so an upgrade cannot leave it stale.
function announcerStyleHash() {
	const kit = dirname(createRequire(import.meta.url).resolve('@sveltejs/kit/package.json'));
	const source = readFileSync(join(kit, 'src/core/sync/write_root.js'), 'utf8');
	const style = /id="svelte-announcer"[^>]*?style="([^"]+)"/.exec(source)?.[1];
	if (!style) throw new Error('svelte.config.js: SvelteKit’s announcer style was not found');
	return `sha256-${createHash('sha256').update(style).digest('base64')}`;
}

/** @type {import('@sveltejs/kit').Config} */
const config = {
	preprocess: vitePreprocess(),
	kit: {
		// A single-page app of static files, served by the Oceans bridge
		// (go/cmd/bridge): every route not a file gets index.html. Gzip
		// copies are served to browsers that accept them.
		adapter: adapter({ pages: 'build', assets: 'build', fallback: 'index.html', precompress: true }),
		// Absolute asset paths: the page is served at every route.
		paths: { relative: false },
		// One script and one stylesheet: the bridge serves one request at a
		// time, so fewer files load faster.
		output: { bundleStrategy: 'single' },
		// The page's policy, with the hash of SvelteKit's inline bootstrap
		// script; the bridge sends it as the Content-Security-Policy header.
		csp: {
			mode: 'hash',
			directives: {
				'default-src': ['self'],
				'script-src': ['self'],
				'style-src': ['self', 'unsafe-hashes', /** @type {`sha256-${string}`} */ (announcerStyleHash())],
				'img-src': ['self', 'data:'],
				'connect-src': ['self'],
				'object-src': ['none'],
				'base-uri': ['none'],
				'form-action': ['self']
			}
		}
	}
};

export default config;
