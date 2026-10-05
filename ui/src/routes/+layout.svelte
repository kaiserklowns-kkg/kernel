<script lang="ts">
	import '../app.css';
	import { onMount, type Snippet } from 'svelte';
	import { page } from '$app/state';
	import PairScreen from '$lib/components/PairScreen.svelte';
	import Spinner from '$lib/components/Spinner.svelte';
	import { pairing } from '$lib/session.svelte';

	let { children }: { children: Snippet } = $props();

	const sections = [
		{ href: '/', label: 'Control Center', icon: 'M4 13h6V4H4zm0 7h6v-5H4zm10 0h6v-9h-6zm0-16v5h6V4z' },
		{ href: '/apps', label: 'Apps', icon: 'M4 4h7v7H4zm9 0h7v7h-7zM4 13h7v7H4zm9 0h7v7h-7z' },
		{
			href: '/store',
			label: 'Store',
			icon: 'M5 8h14l-1.2 11.2a1 1 0 0 1-1 .8H7.2a1 1 0 0 1-1-.8zm4 0V6a3 3 0 0 1 6 0v2'
		},
		{
			href: '/ai',
			label: 'AI Center',
			icon: 'M12 3l1.9 5.1L19 10l-5.1 1.9L12 17l-1.9-5.1L5 10l5.1-1.9zM18 15l.9 2.1L21 18l-2.1.9L18 21l-.9-2.1L15 18l2.1-.9z'
		},
		{
			href: '/settings',
			label: 'Settings',
			icon: 'M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7zm7.4-2.5a7.6 7.6 0 0 0 0-2l2-1.6-2-3.4-2.4 1a7.4 7.4 0 0 0-1.7-1l-.4-2.5h-4l-.4 2.5a7.4 7.4 0 0 0-1.7 1l-2.4-1-2 3.4 2 1.6a7.6 7.6 0 0 0 0 2l-2 1.6 2 3.4 2.4-1a7.4 7.4 0 0 0 1.7 1l.4 2.5h4l.4-2.5a7.4 7.4 0 0 0 1.7-1l2.4 1 2-3.4z'
		}
	];

	function current(href: string): boolean {
		return href === '/' ? page.url.pathname === '/' : page.url.pathname.startsWith(href);
	}

	onMount(() => {
		void pairing.check();
	});
</script>

{#if pairing.state === 'checking'}
	<div class="loading" role="status">
		<Spinner size={22} label="Connecting to Oceans" />
	</div>
{:else if pairing.state !== 'paired'}
	<PairScreen />
{:else}
	<a class="skip" href="#main">Skip to content</a>
	<div class="shell">
		<nav aria-label="Oceans">
			<div class="brand">
				<img src="/favicon.svg" alt="" width="28" height="28" />
				<span>Oceans</span>
			</div>
			<ul>
				{#each sections as section (section.href)}
					<li>
						<a href={section.href} aria-current={current(section.href) ? 'page' : undefined}>
							<svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true"><path d={section.icon} /></svg>
							{section.label}
						</a>
					</li>
				{/each}
			</ul>
		</nav>
		<main id="main" tabindex="-1">
			{@render children()}
		</main>
	</div>
{/if}

<style>
	.loading {
		min-height: 100vh;
		display: grid;
		place-items: center;
		color: var(--text-muted);
	}

	.skip {
		position: absolute;
		left: var(--space-3);
		top: -48px;
		padding: var(--space-2) var(--space-3);
		background: var(--surface-3);
		border-radius: var(--radius-s);
		z-index: 10;
	}

	.skip:focus-visible {
		top: var(--space-3);
	}

	.shell {
		display: grid;
		grid-template-columns: 232px minmax(0, 1fr);
		min-height: 100vh;
	}

	nav {
		position: sticky;
		top: 0;
		align-self: start;
		height: 100vh;
		display: flex;
		flex-direction: column;
		gap: var(--space-5);
		padding: var(--space-5) var(--space-3);
		border-right: 1px solid var(--border);
		background: var(--surface-1);
	}

	.brand {
		display: flex;
		align-items: center;
		gap: var(--space-3);
		padding: 0 var(--space-3);
		font-weight: 650;
		font-size: var(--text-l);
		letter-spacing: -0.01em;
	}

	ul {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		gap: 2px;
	}

	nav a {
		display: flex;
		align-items: center;
		gap: var(--space-3);
		padding: var(--space-2) var(--space-3);
		border-radius: var(--radius-m);
		color: var(--text-muted);
		font-size: var(--text-s);
		font-weight: 550;
		transition:
			background var(--speed) ease,
			color var(--speed) ease;
	}

	nav a svg {
		fill: currentColor;
		opacity: 0.85;
	}

	nav a:hover {
		background: var(--surface-2);
		color: var(--text);
	}

	nav a[aria-current='page'] {
		background: var(--accent-soft);
		color: var(--accent);
	}

	main {
		padding: var(--space-6) var(--space-6) var(--space-7);
		max-width: 1120px;
		width: 100%;
		outline: none;
	}

	@media (max-width: 760px) {
		.shell {
			grid-template-columns: 1fr;
		}

		nav {
			position: static;
			height: auto;
			flex-direction: row;
			align-items: center;
			justify-content: space-between;
			padding: var(--space-3);
			border-right: none;
			border-bottom: 1px solid var(--border);
			overflow-x: auto;
		}

		.brand span {
			display: none;
		}

		ul {
			display: flex;
		}

		main {
			padding: var(--space-5) var(--space-4);
		}
	}
</style>
