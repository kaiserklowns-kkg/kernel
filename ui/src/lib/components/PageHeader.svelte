<script lang="ts">
	import type { Snippet } from 'svelte';

	let { title, description = '', actions }: { title: string; description?: string; actions?: Snippet } = $props();
</script>

<header>
	<div>
		<h1>{title}</h1>
		{#if description}<p class="muted">{description}</p>{/if}
	</div>
	{#if actions}<div class="actions">{@render actions()}</div>{/if}
</header>

<style>
	header {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		gap: var(--space-4);
		margin-bottom: var(--space-6);
		flex-wrap: wrap;
	}

	h1 {
		font-size: var(--text-2xl);
		letter-spacing: -0.02em;
	}

	p {
		margin-top: var(--space-1);
		max-width: 60ch;
	}

	.actions {
		display: flex;
		gap: var(--space-2);
		align-items: center;
	}
</style>
