<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLButtonAttributes } from 'svelte/elements';
	import Spinner from './Spinner.svelte';

	type Variant = 'primary' | 'secondary' | 'danger' | 'ghost';

	interface Props extends Omit<HTMLButtonAttributes, 'children'> {
		variant?: Variant;
		size?: 'm' | 's';
		/** Working: disabled, with a spinner and aria-busy. */
		loading?: boolean;
		children: Snippet;
	}

	let {
		variant = 'secondary',
		size = 'm',
		loading = false,
		disabled = false,
		type = 'button',
		children,
		...rest
	}: Props = $props();
</script>

<button
	{...rest}
	{type}
	class="button {variant} {size}"
	disabled={disabled || loading}
	aria-busy={loading ? 'true' : undefined}
>
	{#if loading}<Spinner size={14} />{/if}
	<span class="label">{@render children()}</span>
</button>

<style>
	.button {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: var(--space-2);
		min-height: 36px;
		padding: 0 var(--space-4);
		border: 1px solid var(--border-strong);
		border-radius: var(--radius-m);
		background: var(--surface-3);
		color: var(--text);
		font: inherit;
		font-size: var(--text-s);
		font-weight: 550;
		cursor: pointer;
		transition:
			background var(--speed) ease,
			border-color var(--speed) ease,
			color var(--speed) ease,
			transform var(--speed) ease;
		white-space: nowrap;
	}

	.button.s {
		min-height: 30px;
		padding: 0 var(--space-3);
		border-radius: var(--radius-s);
	}

	.button:hover:not(:disabled) {
		background: var(--surface-hover);
		border-color: var(--text-faint);
	}

	.button:active:not(:disabled) {
		transform: translateY(1px);
	}

	.button:disabled {
		cursor: not-allowed;
		opacity: 0.5;
	}

	.button[aria-busy='true'] {
		cursor: progress;
		opacity: 0.8;
	}

	.primary {
		background: var(--accent);
		border-color: var(--accent);
		color: var(--on-accent);
	}

	.primary:hover:not(:disabled) {
		background: var(--accent-hover);
		border-color: var(--accent-hover);
	}

	.primary:active:not(:disabled) {
		background: var(--accent-active);
	}

	.danger {
		background: transparent;
		border-color: color-mix(in srgb, var(--error) 55%, transparent);
		color: var(--error);
	}

	.danger:hover:not(:disabled) {
		background: var(--error-soft);
		border-color: var(--error);
	}

	.ghost {
		background: transparent;
		border-color: transparent;
		color: var(--text-muted);
	}

	.ghost:hover:not(:disabled) {
		background: var(--surface-3);
		border-color: transparent;
		color: var(--text);
	}
</style>
