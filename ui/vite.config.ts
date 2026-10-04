import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// `bun run dev` serves the app on the development machine; API calls go
// to an Oceans bridge (OCEANS_BRIDGE, e.g. http://127.0.0.1:8080, the port
// `cargo xtask run` forwards).
export default defineConfig({
	plugins: [sveltekit()],
	server: {
		proxy: {
			'/api': { target: process.env.OCEANS_BRIDGE ?? 'http://127.0.0.1:8080', changeOrigin: true }
		}
	},
	build: { target: 'es2022' }
});
