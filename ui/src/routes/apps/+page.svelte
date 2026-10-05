<script lang="ts">
	import { onMount } from 'svelte';
	import { SvelteMap } from 'svelte/reactivity';
	import { api, type App, type Permission } from '$lib/api';
	import Badge from '$lib/components/Badge.svelte';
	import Button from '$lib/components/Button.svelte';
	import Card from '$lib/components/Card.svelte';
	import Notice from '$lib/components/Notice.svelte';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import Spinner from '$lib/components/Spinner.svelte';
	import { decision, kind, runtime, webAppURL } from '$lib/format';
	import { poll } from '$lib/poll';
	import { describe, pairing } from '$lib/session.svelte';

	let apps = $state<App[] | null>(null);
	let error = $state('');
	let refreshing = $state(false);
	/** An app being started or stopped. */
	let busy = $state('');
	/** The last start or stop, per app: what happened. */
	const outcomes = new SvelteMap<string, { tone: 'success' | 'error'; text: string }>();
	/** Permissions per app, loaded when shown. */
	const permissions = new SvelteMap<string, Permission[] | string>();
	const open = new SvelteMap<string, boolean>();
	let audit = $state<string[] | null>(null);
	let auditError = $state('');

	async function refresh(manual = false) {
		refreshing = manual;
		try {
			apps = await pairing.guard(() => api.apps());
			error = '';
		} catch (e) {
			error = describe(e);
		} finally {
			refreshing = false;
		}
	}

	async function change(app: App, start: boolean) {
		busy = app.id;
		outcomes.delete(app.id);
		try {
			await pairing.guard(() => (start ? api.start(app.id) : api.stop(app.id)));
			outcomes.set(app.id, { tone: 'success', text: start ? 'Started.' : 'Stopped.' });
		} catch (e) {
			outcomes.set(app.id, { tone: 'error', text: describe(e) });
		} finally {
			busy = '';
			await refresh();
		}
	}

	async function togglePermissions(app: App) {
		const shown = !open.get(app.id);
		open.set(app.id, shown);
		if (shown) {
			try {
				permissions.set(app.id, await pairing.guard(() => api.permissions(app.id)));
			} catch (e) {
				permissions.set(app.id, describe(e));
			}
		}
	}

	async function loadAudit() {
		try {
			audit = await pairing.guard(() => api.audit());
			auditError = '';
		} catch (e) {
			auditError = describe(e);
		}
	}

	onMount(() => {
		void refresh();
		return poll(refresh, 10000);
	});
</script>

<svelte:head><title>Apps · Oceans</title></svelte:head>

<PageHeader
	title="Apps"
	description="Installed apps, what they may do, and whether they run. Permission decisions are made on the Oceans console."
>
	{#snippet actions()}
		<Button size="s" variant="ghost" loading={refreshing} onclick={() => refresh(true)}>Refresh</Button>
	{/snippet}
</PageHeader>

{#if error}
	<div class="spaced"><Notice tone="error" title="Could not list the apps">{error}</Notice></div>
{/if}

{#if apps === null}
	{#if !error}<div class="placeholder"><Spinner size={20} label="Loading" /></div>{/if}
{:else if apps.length === 0}
	<Card>
		<p class="muted">No apps are installed. Install signed packages on the console with <kbd>app install PATH</kbd>.</p>
	</Card>
{:else}
	<ul class="apps">
		{#each apps as app (app.id)}
			{@const outcome = outcomes.get(app.id)}
			{@const listed = permissions.get(app.id)}
			<li>
				<Card>
					<div class="app">
						<div class="identity">
							<div class="icon" aria-hidden="true">{app.name.charAt(0).toUpperCase()}</div>
							<div class="names">
								<h2>{app.name}</h2>
								<p class="mono faint">{app.id}</p>
							</div>
						</div>
						<div class="state">
							{#if app.running}
								<Badge tone="success" dot>Running</Badge>
							{:else}
								<Badge>Installed</Badge>
							{/if}
						</div>
						<div class="controls">
							{#if app.runtime === 'web'}
								<!-- A web app (ADR-0064) runs in the browser, from its own origin. -->
								<a class="open" href={webAppURL(app.id, location)} target="_blank" rel="noopener noreferrer">Open</a>
							{:else if app.running}
								<Button variant="danger" size="s" loading={busy === app.id} disabled={busy !== '' && busy !== app.id} onclick={() => change(app, false)}>Stop</Button>
							{:else}
								<Button variant="primary" size="s" loading={busy === app.id} disabled={busy !== '' && busy !== app.id} onclick={() => change(app, true)}>Start</Button>
							{/if}
						</div>
					</div>
					<dl class="facts">
						<div><dt>Version</dt><dd>{app.version}</dd></div>
						<div><dt>Kind</dt><dd>{kind(app.kind) || '—'}</dd></div>
						<div><dt>Runtime</dt><dd>{runtime(app.runtime) || '—'}</dd></div>
						<div><dt>Publisher</dt><dd>{app.publisher || '—'}</dd></div>
					</dl>
					{#if outcome}
						<Notice tone={outcome.tone}>{outcome.text}</Notice>
					{/if}
					<div>
						<Button
							variant="ghost"
							size="s"
							aria-expanded={open.get(app.id) ? 'true' : 'false'}
							aria-controls="permissions-{app.id}"
							onclick={() => togglePermissions(app)}
						>
							{open.get(app.id) ? 'Hide permissions' : 'Permissions'}
						</Button>
					</div>
					{#if open.get(app.id)}
						<div id="permissions-{app.id}">
							{#if listed === undefined}
								<Spinner size={16} label="Loading permissions" />
							{:else if typeof listed === 'string'}
								<Notice tone="error">{listed}</Notice>
							{:else if listed.length === 0}
								<p class="muted small">This app asks for no permissions.</p>
							{:else}
								<ul class="permissions">
									{#each listed as permission (permission.name)}
										{@const word = decision(permission.decision)}
										<li>
											<div>
												<p class="permission-name">{permission.name}</p>
												<p class="muted small">May {permission.description}.</p>
												{#if permission.reason}
													<blockquote class="small">“{permission.reason}”<span class="faint"> — the app’s reason</span></blockquote>
												{/if}
											</div>
											<Badge tone={word.tone}>{word.label}</Badge>
										</li>
									{/each}
								</ul>
							{/if}
						</div>
					{/if}
				</Card>
			</li>
		{/each}
	</ul>
{/if}

<div class="audit">
	<Card title="Audit log" subtitle="What was installed, run and decided, newest first.">
		{#snippet actions()}
			<Button size="s" onclick={loadAudit}>{audit === null ? 'Show' : 'Refresh'}</Button>
		{/snippet}
		{#if auditError}
			<Notice tone="error">{auditError}</Notice>
		{:else if audit !== null}
			{#if audit.length === 0}
				<p class="muted small">Nothing yet.</p>
			{:else}
				<ol class="log">
					{#each audit as entry, i (i)}
						<li class="mono">{entry}</li>
					{/each}
				</ol>
			{/if}
		{/if}
	</Card>
</div>

<style>
	.spaced {
		margin-bottom: var(--space-5);
	}

	.placeholder {
		display: grid;
		place-items: center;
		padding: var(--space-7);
		color: var(--text-muted);
	}

	.apps {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		gap: var(--space-4);
	}

	.app {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto auto;
		align-items: center;
		gap: var(--space-4);
	}

	.identity {
		display: flex;
		align-items: center;
		gap: var(--space-3);
		min-width: 0;
	}

	.icon {
		flex: none;
		width: 40px;
		height: 40px;
		display: grid;
		place-items: center;
		border-radius: var(--radius-m);
		background: linear-gradient(140deg, var(--surface-3), var(--accent-soft));
		border: 1px solid var(--border-strong);
		color: var(--accent);
		font-weight: 650;
	}

	.names {
		min-width: 0;
	}

	h2 {
		font-size: var(--text-l);
	}

	.names p {
		font-size: var(--text-s);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.open {
		display: inline-flex;
		align-items: center;
		min-height: 32px;
		padding: 0 var(--space-4);
		border-radius: var(--radius-m);
		background: var(--accent);
		color: var(--on-accent);
		font-weight: 600;
		text-decoration: none;
	}

	.open:hover {
		background: var(--accent-hover);
	}

	.facts {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
		gap: var(--space-3);
		margin: 0;
		padding: var(--space-3) 0 0;
		border-top: 1px solid var(--border);
	}

	dt {
		font-size: var(--text-xs);
		color: var(--text-faint);
		text-transform: uppercase;
		letter-spacing: 0.06em;
	}

	dd {
		margin: 2px 0 0;
		font-size: var(--text-s);
	}

	.permissions {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		gap: var(--space-2);
	}

	.permissions li {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: var(--space-4);
		padding: var(--space-3);
		border-radius: var(--radius-m);
		background: var(--surface-2);
	}

	.permission-name {
		font-weight: 600;
		font-family: var(--mono);
		font-size: var(--text-s);
	}

	blockquote {
		margin: var(--space-1) 0 0;
		padding-left: var(--space-3);
		border-left: 2px solid var(--border-strong);
		color: var(--text-muted);
	}

	.small {
		font-size: var(--text-s);
	}

	.audit {
		margin-top: var(--space-5);
	}

	.log {
		margin: 0;
		padding: 0;
		list-style: none;
		display: grid;
		gap: 2px;
		font-size: var(--text-xs);
		color: var(--text-muted);
		max-height: 320px;
		overflow: auto;
	}

	.log li {
		padding: var(--space-1) var(--space-2);
		border-radius: var(--radius-s);
		overflow-wrap: anywhere;
	}

	.log li:nth-child(odd) {
		background: var(--surface-2);
	}

	@media (max-width: 560px) {
		.app {
			grid-template-columns: minmax(0, 1fr) auto;
		}

		.state {
			display: none;
		}
	}
</style>
