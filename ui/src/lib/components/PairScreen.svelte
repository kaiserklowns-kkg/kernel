<script lang="ts">
	import Button from './Button.svelte';
	import Notice from './Notice.svelte';
	import TextField from './TextField.svelte';
	import { normalizeCode, plausibleCode } from '$lib/format';
	import { describe, pairing } from '$lib/session.svelte';

	let code = $state('');
	let error = $state('');
	let busy = $state(false);

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		const normalized = normalizeCode(code);
		if (!plausibleCode(normalized)) {
			error = 'A pairing code is 32 hexadecimal digits, as ui pair prints it.';
			return;
		}
		error = '';
		busy = true;
		try {
			await pairing.login(normalized);
			code = '';
		} catch (e) {
			error = describe(e);
		} finally {
			busy = false;
		}
	}
</script>

<main class="pair">
	<div class="panel">
		<div class="brand">
			<img src="/favicon.svg" alt="" width="40" height="40" />
			<div>
				<h1>Oceans</h1>
				<p class="muted">Pair this browser with your computer</p>
			</div>
		</div>

		{#if pairing.state === 'unreachable'}
			<Notice tone="error" title="Oceans is not answering">{pairing.message}</Notice>
			<Button onclick={() => pairing.check()}>Try again</Button>
		{:else}
			<ol class="steps">
				<li>On the Oceans console, type <kbd>ui pair</kbd>.</li>
				<li>Enter the pairing code it shows here.</li>
			</ol>
			{#if !pairing.systemPaired}
				<Notice tone="info">No browser is paired with Oceans right now.</Notice>
			{/if}
			<form onsubmit={submit} novalidate>
				<TextField
					label="Pairing code"
					bind:value={code}
					placeholder="32 hexadecimal digits"
					autocomplete="off"
					autocapitalize="off"
					spellcheck="false"
					inputmode="text"
					class="mono"
					{error}
				/>
				<Button type="submit" variant="primary" loading={busy}>Pair this browser</Button>
			</form>
			<p class="faint small">
				A paired browser can see your system, start and stop apps and ask Oceans AI. It can never install
				apps or change their permissions. Type <kbd>ui unpair</kbd> to end it.
			</p>
		{/if}
	</div>
</main>

<style>
	.pair {
		min-height: 100vh;
		display: grid;
		place-items: center;
		padding: var(--space-5);
		background:
			radial-gradient(1200px 600px at 50% -10%, rgb(91 156 248 / 0.08), transparent 60%),
			var(--bg);
	}

	.panel {
		width: min(440px, 100%);
		display: flex;
		flex-direction: column;
		gap: var(--space-5);
		padding: var(--space-6);
		border: 1px solid var(--border);
		border-radius: var(--radius-l);
		background: var(--surface-1);
		box-shadow: var(--shadow);
	}

	.brand {
		display: flex;
		align-items: center;
		gap: var(--space-3);
	}

	h1 {
		font-size: var(--text-xl);
	}

	.steps {
		margin: 0;
		padding-left: 1.25rem;
		color: var(--text-muted);
		display: grid;
		gap: var(--space-2);
	}

	form {
		display: flex;
		flex-direction: column;
		gap: var(--space-4);
	}

	.small {
		font-size: var(--text-s);
	}
</style>
