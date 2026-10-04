<script lang="ts">
	import { onMount } from 'svelte';
	import { api, type SystemInfo } from '$lib/api';
	import Button from '$lib/components/Button.svelte';
	import Card from '$lib/components/Card.svelte';
	import Notice from '$lib/components/Notice.svelte';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import TextField from '$lib/components/TextField.svelte';
	import { size, uptime } from '$lib/format';
	import { describe, pairing } from '$lib/session.svelte';

	let url = $state('');
	let model = $state('');
	let urlError = $state('');
	let modelError = $state('');
	let saving = $state(false);
	let saved = $state<{ tone: 'success' | 'error' | 'warning'; text: string } | null>(null);
	let info = $state<SystemInfo | null>(null);
	let infoError = $state('');
	let leaving = $state(false);

	function validate(): boolean {
		urlError = /^https?:\/\/\S+$/.test(url.trim()) ? '' : 'An http:// address, e.g. http://192.168.1.10:11434/v1';
		modelError = /^\S+$/.test(model.trim()) ? '' : 'The model’s name, e.g. llama3.1';
		return urlError === '' && modelError === '';
	}

	async function save(event: SubmitEvent) {
		event.preventDefault();
		saved = null;
		if (!validate()) return;
		saving = true;
		try {
			const result = await pairing.guard(() => api.setModel(url, model));
			saved = result.note
				? { tone: 'warning', text: result.note }
				: { tone: 'success', text: `Oceans AI now uses ${result.model} at ${result.url}.` };
		} catch (e) {
			saved = { tone: 'error', text: describe(e) };
		} finally {
			saving = false;
		}
	}

	async function leave() {
		leaving = true;
		try {
			await pairing.logout();
		} finally {
			leaving = false;
		}
	}

	onMount(async () => {
		try {
			info = await pairing.guard(() => api.system());
		} catch (e) {
			infoError = describe(e);
		}
	});
</script>

<svelte:head><title>Settings · Oceans</title></svelte:head>

<PageHeader title="Settings" />

<div class="grid">
	<Card
		title="AI model"
		subtitle="The model server Oceans AI asks: any OpenAI-compatible endpoint, such as Ollama or llama.cpp on your network."
	>
		<form onsubmit={save} novalidate>
			<TextField
				label="Server address"
				bind:value={url}
				placeholder="http://192.168.1.10:11434/v1"
				autocomplete="off"
				spellcheck="false"
				error={urlError}
			/>
			<TextField
				label="Model"
				bind:value={model}
				placeholder="llama3.1"
				autocomplete="off"
				spellcheck="false"
				error={modelError}
			/>
			<div class="row">
				<Button type="submit" variant="primary" loading={saving}>Save</Button>
			</div>
			{#if saved}<Notice tone={saved.tone}>{saved.text}</Notice>{/if}
		</form>
		<p class="faint small">What you ask is sent to this server. The current setting is kept by the AI service across restarts.</p>
	</Card>

	<Card title="About Oceans">
		{#if infoError}
			<Notice tone="error">{infoError}</Notice>
		{:else if info}
			<dl>
				<div><dt>Kernel</dt><dd class="mono">{info.kernel.version || '—'}</dd></div>
				<div><dt>Architecture</dt><dd class="mono">{info.kernel.arch || '—'}</dd></div>
				<div><dt>System call ABI</dt><dd class="mono">{info.kernel.abi}</dd></div>
				<div><dt>Memory</dt><dd>{size(info.memory.totalMiB * 1024)}</dd></div>
				<div><dt>Up for</dt><dd>{uptime(info.uptimeSeconds)}</dd></div>
			</dl>
		{/if}
		<p class="muted small">
			A capability-based operating system: every program, service and AI agent holds only what it was given. This
			page reaches Oceans through its bridge service, with the capability your console granted when you paired.
		</p>
	</Card>

	<Card title="This browser" subtitle="Paired with Oceans. Typing ui unpair on the console ends it for every browser.">
		<div class="row">
			<Button variant="danger" loading={leaving} onclick={leave}>Sign out of this browser</Button>
		</div>
	</Card>
</div>

<style>
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
		gap: var(--space-4);
		align-items: start;
	}

	form {
		display: grid;
		gap: var(--space-4);
	}

	.row {
		display: flex;
		gap: var(--space-2);
	}

	.small {
		font-size: var(--text-s);
	}

	dl {
		margin: 0;
		display: grid;
		gap: var(--space-2);
	}

	dl div {
		display: flex;
		justify-content: space-between;
		gap: var(--space-4);
		padding-bottom: var(--space-2);
		border-bottom: 1px solid var(--border);
	}

	dt {
		color: var(--text-muted);
		font-size: var(--text-s);
	}

	dd {
		margin: 0;
		font-size: var(--text-s);
	}
</style>
