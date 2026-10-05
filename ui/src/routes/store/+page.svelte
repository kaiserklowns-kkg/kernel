<script lang="ts">
	import { onMount } from 'svelte';
	import { SvelteMap } from 'svelte/reactivity';
	import { api, type StoreApp, type StoreView } from '$lib/api';
	import Badge from '$lib/components/Badge.svelte';
	import Button from '$lib/components/Button.svelte';
	import Card from '$lib/components/Card.svelte';
	import Notice from '$lib/components/Notice.svelte';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import Spinner from '$lib/components/Spinner.svelte';
	import TextField from '$lib/components/TextField.svelte';
	import { size } from '$lib/format';
	import { describe, pairing } from '$lib/session.svelte';

	let view = $state<StoreView | null>(null);
	let error = $state('');
	let refreshing = $state(false);
	let source = $state('');
	let savingSource = $state(false);
	let sourceOutcome = $state<{ tone: 'success' | 'error'; text: string } | null>(null);
	/** The app whose install is being proposed. */
	let busy = $state('');
	const outcomes = new SvelteMap<string, { tone: 'success' | 'error' | 'info'; text: string }>();

	async function refresh(manual = false) {
		refreshing = manual;
		try {
			view = await pairing.guard(() => api.store());
			if (source === '') source = view.source;
			error = '';
		} catch (e) {
			error = describe(e);
		} finally {
			refreshing = false;
		}
	}

	async function saveSource(event: SubmitEvent) {
		event.preventDefault();
		savingSource = true;
		sourceOutcome = null;
		try {
			await pairing.guard(() => api.setStoreSource(source));
			sourceOutcome = { tone: 'success', text: source.trim() === '' ? 'No store is set.' : 'Saved.' };
			await refresh();
		} catch (e) {
			sourceOutcome = { tone: 'error', text: describe(e) };
		} finally {
			savingSource = false;
		}
	}

	async function install(app: StoreApp) {
		busy = app.id;
		outcomes.delete(app.id);
		try {
			await pairing.guard(() => api.install(app.id));
			outcomes.set(app.id, {
				tone: 'info',
				text: `Confirm on the Oceans screen: it asks before ${app.name} is installed.`
			});
		} catch (e) {
			outcomes.set(app.id, { tone: 'error', text: describe(e) });
		} finally {
			busy = '';
		}
	}

	const labels = { available: 'Install', update: 'Update', installed: 'Installed' } as const;

	onMount(() => {
		void refresh();
	});
</script>

<svelte:head><title>Store · Oceans</title></svelte:head>

<PageHeader
	title="Store"
	description="Apps from the store you choose. Every package must be signed by a publisher Oceans trusts, and Oceans asks on its own screen before installing anything."
>
	{#snippet actions()}
		<Button size="s" variant="ghost" loading={refreshing} onclick={() => refresh(true)}>Refresh</Button>
	{/snippet}
</PageHeader>

<Card>
	<form class="source" onsubmit={saveSource}>
		<TextField label="Store URL" bind:value={source} placeholder="https://store.example/apps" />
		<Button type="submit" size="s" loading={savingSource}>Save</Button>
	</form>
	{#if sourceOutcome}
		<Notice tone={sourceOutcome.tone}>{sourceOutcome.text}</Notice>
	{/if}
</Card>

{#if error}
	<div class="spaced"><Notice tone="error" title="Could not reach the store">{error}</Notice></div>
{/if}

{#if view === null}
	{#if !error}<div class="placeholder"><Spinner size={20} label="Loading" /></div>{/if}
{:else if view.source === ''}
	<Card><p class="muted">No store is set. Enter a store's URL above to see its apps.</p></Card>
{:else if view.apps.length === 0}
	<Card><p class="muted">This store lists no apps.</p></Card>
{:else}
	<ul class="apps">
		{#each view.apps as app (app.id)}
			{@const outcome = outcomes.get(app.id)}
			<li>
				<Card>
					<div class="app">
						<div class="identity">
							<div class="icon" aria-hidden="true">{app.name.charAt(0).toUpperCase()}</div>
							<div class="names">
								<h2>{app.name} <span class="faint">{app.version}</span></h2>
								<p class="faint">{app.publisher} · {size(Math.ceil(app.size / 1024))}</p>
							</div>
						</div>
						<div class="controls">
							{#if app.state === 'installed'}
								<Badge tone="success" dot>Installed</Badge>
							{:else}
								<Button
									variant="primary"
									size="s"
									loading={busy === app.id}
									disabled={busy !== '' && busy !== app.id}
									onclick={() => install(app)}>{labels[app.state]}</Button
								>
							{/if}
						</div>
					</div>
					{#if app.description}<p class="description">{app.description}</p>{/if}
					<p class="faint asks">
						{app.permissions.length === 0 ? 'Asks for no permissions.' : `Asks for: ${app.permissions.join(', ')}`}
					</p>
					{#if outcome}
						<Notice tone={outcome.tone}>{outcome.text}</Notice>
					{/if}
				</Card>
			</li>
		{/each}
	</ul>
{/if}

<style>
	.spaced {
		margin: var(--space-5) 0;
	}

	.placeholder {
		display: grid;
		place-items: center;
		padding: var(--space-7);
		color: var(--text-muted);
	}

	.source {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		align-items: end;
		gap: var(--space-3);
	}

	.apps {
		list-style: none;
		margin: var(--space-5) 0 0;
		padding: 0;
		display: grid;
		gap: var(--space-4);
	}

	.app {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
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

	.names p,
	.asks {
		font-size: var(--text-s);
	}

	.description {
		margin: var(--space-3) 0 var(--space-1);
	}

	@media (max-width: 560px) {
		.source,
		.app {
			grid-template-columns: minmax(0, 1fr);
		}
	}
</style>
