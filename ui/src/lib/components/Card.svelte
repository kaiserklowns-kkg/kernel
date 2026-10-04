<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		title = '',
		subtitle = '',
		actions,
		children
	}: { title?: string; subtitle?: string; actions?: Snippet; children: Snippet } = $props();
</script>

<section class="card">
	{#if title || actions}
		<header>
			<div class="titles">
				{#if title}<h2>{title}</h2>{/if}
				{#if subtitle}<p class="subtitle">{subtitle}</p>{/if}
			</div>
			{#if actions}<div class="actions">{@render actions()}</div>{/if}
		</header>
	{/if}
	{@render children()}
</section>

<style>
	.card {
		display: flex;
		flex-direction: column;
		gap: var(--space-4);
		padding: var(--space-5);
		border: 1px solid var(--border);
		border-radius: var(--radius-l);
		background: var(--surface-1);
		box-shadow: var(--shadow);
		min-width: 0;
	}

	header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: var(--space-4);
	}

	h2 {
		font-size: var(--text-m);
	}

	.subtitle {
		margin-top: 2px;
		color: var(--text-muted);
		font-size: var(--text-s);
	}

	.actions {
		display: flex;
		gap: var(--space-2);
		flex: none;
	}
</style>
