<script lang="ts">
	import type { HTMLInputAttributes } from 'svelte/elements';

	interface Props extends Omit<HTMLInputAttributes, 'value'> {
		label: string;
		value: string;
		hint?: string;
		/** Shown under the field; marks it invalid. */
		error?: string;
	}

	let { label, value = $bindable(''), hint = '', error = '', id, ...rest }: Props = $props();

	const uid = $props.id();
	const inputId = $derived(id ?? `field-${uid}`);
	const describedBy = $derived(error ? `${inputId}-error` : hint ? `${inputId}-hint` : undefined);
</script>

<div class="field" class:invalid={!!error}>
	<label for={inputId}>{label}</label>
	<input
		{...rest}
		id={inputId}
		bind:value
		aria-invalid={error ? 'true' : undefined}
		aria-describedby={describedBy}
	/>
	{#if error}
		<p class="message error" id="{inputId}-error">{error}</p>
	{:else if hint}
		<p class="message" id="{inputId}-hint">{hint}</p>
	{/if}
</div>

<style>
	.field {
		display: flex;
		flex-direction: column;
		gap: 6px;
		min-width: 0;
	}

	label {
		font-size: var(--text-s);
		font-weight: 550;
		color: var(--text-muted);
	}

	input {
		height: 38px;
		padding: 0 var(--space-3);
		border: 1px solid var(--border-strong);
		border-radius: var(--radius-m);
		background: var(--surface-2);
		color: var(--text);
		font: inherit;
		font-size: var(--text-m);
		transition:
			border-color var(--speed) ease,
			background var(--speed) ease;
	}

	input::placeholder {
		color: var(--text-faint);
	}

	input:hover:not(:disabled) {
		border-color: var(--text-faint);
	}

	input:focus-visible {
		border-color: var(--accent);
		border-radius: var(--radius-m);
	}

	input:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}

	.invalid input {
		border-color: var(--error);
	}

	.message {
		font-size: var(--text-xs);
		color: var(--text-faint);
	}

	.message.error {
		color: var(--error);
	}
</style>
