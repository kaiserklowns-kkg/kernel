<script lang="ts">
	import type { Snippet } from 'svelte';

	type Tone = 'info' | 'success' | 'warning' | 'error';

	let { tone = 'info', title = '', children }: { tone?: Tone; title?: string; children: Snippet } = $props();
</script>

<!-- Errors interrupt (alert); the rest is announced politely (status). -->
<div class="notice {tone}" role={tone === 'error' ? 'alert' : 'status'}>
	<span class="bar" aria-hidden="true"></span>
	<div>
		{#if title}<p class="title">{title}</p>{/if}
		<div class="body">{@render children()}</div>
	</div>
</div>

<style>
	.notice {
		display: flex;
		gap: var(--space-3);
		padding: var(--space-3) var(--space-4);
		border-radius: var(--radius-m);
		border: 1px solid var(--border);
		background: var(--surface-2);
		font-size: var(--text-s);
	}

	.bar {
		flex: none;
		width: 3px;
		border-radius: 2px;
		background: currentColor;
	}

	.title {
		font-weight: 600;
		color: var(--text);
	}

	.body {
		color: var(--text-muted);
		overflow-wrap: anywhere;
	}

	.info {
		color: var(--info);
		background: var(--info-soft);
	}

	.success {
		color: var(--success);
		background: var(--success-soft);
	}

	.warning {
		color: var(--warning);
		background: var(--warning-soft);
	}

	.error {
		color: var(--error);
		background: var(--error-soft);
	}
</style>
