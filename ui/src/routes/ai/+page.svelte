<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { api, maxQuestion, type AISession } from '$lib/api';
	import Badge from '$lib/components/Badge.svelte';
	import Button from '$lib/components/Button.svelte';
	import Card from '$lib/components/Card.svelte';
	import Notice from '$lib/components/Notice.svelte';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import Spinner from '$lib/components/Spinner.svelte';
	import { describe, pairing } from '$lib/session.svelte';

	interface Step {
		/** The approval question, in the system's words. */
		question: string;
		decision: 'allowed' | 'denied' | null;
	}

	interface Exchange {
		id: number;
		question: string;
		state: 'asking' | 'needs-approval' | 'answering' | 'done' | 'failed';
		session: number;
		steps: Step[];
		answer: string;
	}

	let question = $state('');
	let error = $state('');
	let exchanges = $state<Exchange[]>([]);
	let activity = $state<string[] | null>(null);
	let activityError = $state('');
	let nextId = 0;

	const asking = $derived(exchanges.some((e) => e.state === 'asking' || e.state === 'answering'));
	const waiting = $derived(exchanges.some((e) => e.state === 'needs-approval'));
	const tooLong = $derived(new TextEncoder().encode(question.trim()).length > maxQuestion);

	function apply(exchange: Exchange, session: AISession) {
		exchange.session = session.session;
		exchange.state = session.state;
		if (session.state === 'needs-approval') {
			exchange.steps.push({ question: session.text, decision: null });
			// The safe answer has the focus: Enter never approves by accident.
			void tick().then(() => document.getElementById(`deny-${exchange.id}`)?.focus());
		} else {
			exchange.answer = session.text;
		}
	}

	async function ask(event: SubmitEvent) {
		event.preventDefault();
		const text = question.trim();
		if (text === '' || tooLong) return;
		error = '';
		const exchange: Exchange = { id: nextId++, question: text, state: 'asking', session: 0, steps: [], answer: '' };
		exchanges.unshift(exchange);
		const live = exchanges[0];
		if (!live) return;
		question = '';
		try {
			apply(live, await pairing.guard(() => api.ask(text)));
		} catch (e) {
			live.state = 'failed';
			live.answer = describe(e);
		}
		void loadActivity();
	}

	/** Only this click answers: it is the user's decision, logged as theirs. */
	async function decide(exchange: Exchange, approve: boolean) {
		const step = exchange.steps.at(-1);
		if (!step || exchange.state !== 'needs-approval') return;
		step.decision = approve ? 'allowed' : 'denied';
		exchange.state = 'answering';
		try {
			apply(exchange, await pairing.guard(() => api.answer(exchange.session, approve)));
		} catch (e) {
			exchange.state = 'failed';
			exchange.answer = describe(e);
		}
		void loadActivity();
	}

	async function loadActivity() {
		try {
			activity = await pairing.guard(() => api.activity());
			activityError = '';
		} catch (e) {
			activityError = describe(e);
		}
	}

	onMount(() => {
		void loadActivity();
	});
</script>

<svelte:head><title>AI Center · Oceans</title></svelte:head>

<PageHeader
	title="AI Center"
	description="Ask Oceans AI about this computer or to act on it. It asks you before it changes anything, in the system's words."
/>

<form class="ask" onsubmit={ask}>
	<label class="visually-hidden" for="question">Your question</label>
	<input
		id="question"
		bind:value={question}
		placeholder="How much memory is free?"
		autocomplete="off"
		maxlength={maxQuestion}
		disabled={waiting}
		aria-invalid={tooLong ? 'true' : undefined}
	/>
	<Button type="submit" variant="primary" loading={asking} disabled={question.trim() === '' || tooLong || waiting}>Ask</Button>
</form>
{#if waiting}
	<p class="hint faint">Answer the question below first.</p>
{/if}
{#if error}<Notice tone="error">{error}</Notice>{/if}

<ol class="exchanges" aria-live="polite">
	{#each exchanges as exchange (exchange.id)}
		<li>
			<Card>
				<p class="question"><span class="faint">You asked</span> {exchange.question}</p>
				{#each exchange.steps as step, i (i)}
					<section
						class="approval"
						class:open={step.decision === null}
						role={step.decision === null ? 'alertdialog' : undefined}
						aria-labelledby="approval-{exchange.id}-{i}"
					>
						<p class="approval-title" id="approval-{exchange.id}-{i}">Oceans AI wants to:</p>
						<p class="approval-text">{step.question}</p>
						{#if step.decision === null}
							<div class="buttons">
								<Button id="deny-{exchange.id}" onclick={() => decide(exchange, false)}>Deny</Button>
								<Button variant="primary" onclick={() => decide(exchange, true)}>Allow</Button>
							</div>
						{:else}
							<Badge tone={step.decision === 'allowed' ? 'success' : 'neutral'}>
								{step.decision === 'allowed' ? 'You allowed this' : 'You denied this'}
							</Badge>
						{/if}
					</section>
				{/each}
				{#if exchange.state === 'asking' || exchange.state === 'answering'}
					<p class="working muted"><Spinner size={14} /> Working…</p>
				{:else if exchange.state === 'done'}
					<p class="answer">{exchange.answer}</p>
				{:else if exchange.state === 'failed'}
					<Notice tone="error">{exchange.answer}</Notice>
				{/if}
			</Card>
		</li>
	{/each}
</ol>

<div class="activity">
	<Card title="Activity" subtitle="Everything Oceans AI did and what was decided, newest first.">
		{#snippet actions()}
			<Button size="s" variant="ghost" onclick={loadActivity}>Refresh</Button>
		{/snippet}
		{#if activityError}
			<Notice tone="error">{activityError}</Notice>
		{:else if activity === null}
			<Spinner size={16} label="Loading activity" />
		{:else if activity.length === 0}
			<p class="muted small">No activity yet.</p>
		{:else}
			<ol class="log">
				{#each activity as entry, i (i)}
					<li class="mono">{entry}</li>
				{/each}
			</ol>
		{/if}
	</Card>
</div>

<style>
	.ask {
		display: flex;
		gap: var(--space-2);
		margin-bottom: var(--space-2);
	}

	.ask input {
		flex: 1;
		min-width: 0;
		height: 44px;
		padding: 0 var(--space-4);
		border: 1px solid var(--border-strong);
		border-radius: var(--radius-m);
		background: var(--surface-1);
		color: var(--text);
		font: inherit;
	}

	.ask input:hover:not(:disabled) {
		border-color: var(--text-faint);
	}

	.ask input:focus-visible {
		border-color: var(--accent);
		border-radius: var(--radius-m);
	}

	.ask input:disabled {
		opacity: 0.55;
	}

	.ask input[aria-invalid='true'] {
		border-color: var(--error);
	}

	.ask :global(.button) {
		min-height: 44px;
	}

	.hint {
		font-size: var(--text-s);
		margin-bottom: var(--space-2);
	}

	.exchanges {
		list-style: none;
		margin: var(--space-5) 0 0;
		padding: 0;
		display: grid;
		gap: var(--space-4);
	}

	.question {
		font-weight: 550;
	}

	.question .faint {
		display: block;
		font-size: var(--text-xs);
		font-weight: 500;
		text-transform: uppercase;
		letter-spacing: 0.06em;
	}

	.approval {
		display: grid;
		gap: var(--space-3);
		padding: var(--space-4);
		border-radius: var(--radius-m);
		border: 1px solid var(--border);
		background: var(--surface-2);
	}

	.approval.open {
		border-color: color-mix(in srgb, var(--warning) 55%, transparent);
		background: var(--warning-soft);
	}

	.approval-title {
		font-size: var(--text-s);
		font-weight: 600;
		color: var(--warning);
	}

	.approval:not(.open) .approval-title {
		color: var(--text-faint);
	}

	.approval-text {
		font-size: var(--text-l);
		font-weight: 550;
		overflow-wrap: anywhere;
	}

	.buttons {
		display: flex;
		gap: var(--space-2);
		justify-content: flex-end;
	}

	.answer {
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		line-height: 1.6;
	}

	.working {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		font-size: var(--text-s);
	}

	.activity {
		margin-top: var(--space-6);
	}

	.small {
		font-size: var(--text-s);
	}

	.log {
		margin: 0;
		padding: 0;
		list-style: none;
		display: grid;
		gap: 2px;
		font-size: var(--text-xs);
		color: var(--text-muted);
		max-height: 360px;
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
</style>
