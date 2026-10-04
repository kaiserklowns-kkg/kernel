<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { Tone } from '$lib/format';

	let { tone = 'neutral', dot = false, children }: { tone?: Tone; dot?: boolean; children: Snippet } = $props();
</script>

<span class="badge {tone}">
	{#if dot}<span class="dot" aria-hidden="true"></span>{/if}
	{@render children()}
</span>

<style>
	.badge {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		padding: 2px 8px;
		border-radius: 999px;
		font-size: var(--text-xs);
		font-weight: 600;
		letter-spacing: 0.01em;
		line-height: 1.6;
		white-space: nowrap;
		background: var(--surface-3);
		color: var(--text-muted);
	}

	.dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: currentColor;
	}

	.accent {
		background: var(--accent-soft);
		color: var(--accent);
	}

	.success {
		background: var(--success-soft);
		color: var(--success);
	}

	.warning {
		background: var(--warning-soft);
		color: var(--warning);
	}

	.error {
		background: var(--error-soft);
		color: var(--error);
	}

	.info {
		background: var(--info-soft);
		color: var(--info);
	}
</style>
