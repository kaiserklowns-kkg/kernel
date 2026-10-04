<script lang="ts">
	import { onMount } from 'svelte';
	import { api, type SystemInfo } from '$lib/api';
	import Button from '$lib/components/Button.svelte';
	import Card from '$lib/components/Card.svelte';
	import Meter from '$lib/components/Meter.svelte';
	import Notice from '$lib/components/Notice.svelte';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import Spinner from '$lib/components/Spinner.svelte';
	import { memoryTone, size, uptime, usedPercent } from '$lib/format';
	import { poll } from '$lib/poll';
	import { describe, pairing } from '$lib/session.svelte';

	let info = $state<SystemInfo | null>(null);
	let error = $state('');
	let refreshing = $state(false);

	const percent = $derived(info ? usedPercent(info.memory) : 0);
	const processMemory = $derived(info ? info.processes.reduce((sum, p) => sum + p.memoryKiB, 0) : 0);

	async function refresh(manual = false) {
		refreshing = manual;
		try {
			info = await pairing.guard(() => api.system());
			error = '';
		} catch (e) {
			error = describe(e);
		} finally {
			refreshing = false;
		}
	}

	onMount(() => {
		void refresh();
		return poll(refresh, 5000);
	});
</script>

<svelte:head><title>Control Center · Oceans</title></svelte:head>

<PageHeader title="Control Center" description="How this computer is doing right now.">
	{#snippet actions()}
		<Button size="s" variant="ghost" loading={refreshing} onclick={() => refresh(true)}>Refresh</Button>
	{/snippet}
</PageHeader>

{#if error}
	<div class="spaced"><Notice tone="error" title="Could not read the system">{error}</Notice></div>
{/if}

{#if info === null}
	{#if !error}<div class="placeholder"><Spinner size={20} label="Loading" /></div>{/if}
{:else}
	<div class="stats">
		<Card title="Memory">
			<div class="figure">
				<span class="value">{size((info.memory.totalMiB - info.memory.freeMiB) * 1024)}</span>
				<span class="muted">used of {size(info.memory.totalMiB * 1024)}</span>
			</div>
			<Meter value={percent} label="Memory in use" tone={memoryTone(percent)} />
			<p class="faint small">{size(info.memory.freeMiB * 1024)} free · kernel heap {size(info.memory.kernelHeapKiB)}</p>
		</Card>
		<Card title="Uptime">
			<div class="figure"><span class="value">{uptime(info.uptimeSeconds)}</span></div>
			<p class="faint small">since Oceans started</p>
		</Card>
		<Card title="Processes">
			<div class="figure"><span class="value">{info.processes.length}</span><span class="muted">running</span></div>
			<p class="faint small">{size(processMemory)} of user memory</p>
		</Card>
	</div>

	<Card title="Running processes" subtitle="Largest first. Every process holds only the capabilities it was given.">
		<div class="table-wrap">
			<table>
				<thead>
					<tr>
						<th scope="col">Name</th>
						<th scope="col" class="num">ID</th>
						<th scope="col" class="num">Parent</th>
						<th scope="col" class="num">Memory</th>
					</tr>
				</thead>
				<tbody>
					{#each info.processes as process (process.id)}
						<tr>
							<td class="mono">{process.name}</td>
							<td class="num faint">{process.id}</td>
							<td class="num faint">{process.parent || '—'}</td>
							<td class="num">{size(process.memoryKiB)}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	</Card>
{/if}

<style>
	.spaced {
		margin-bottom: var(--space-5);
	}

	.placeholder {
		display: grid;
		place-items: center;
		padding: var(--space-7);
		color: var(--text-muted);
	}

	.stats {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
		gap: var(--space-4);
		margin-bottom: var(--space-4);
	}

	.figure {
		display: flex;
		align-items: baseline;
		gap: var(--space-2);
		flex-wrap: wrap;
	}

	.value {
		font-size: var(--text-2xl);
		font-weight: 600;
		letter-spacing: -0.02em;
		font-variant-numeric: tabular-nums;
	}

	.small {
		font-size: var(--text-s);
	}

	.table-wrap {
		overflow-x: auto;
		margin: 0 calc(-1 * var(--space-2));
	}

	table {
		width: 100%;
		border-collapse: collapse;
		font-size: var(--text-s);
	}

	th {
		text-align: left;
		font-weight: 550;
		color: var(--text-faint);
		padding: var(--space-2);
		border-bottom: 1px solid var(--border);
	}

	td {
		padding: var(--space-2);
		border-bottom: 1px solid var(--border);
	}

	tbody tr:last-child td {
		border-bottom: none;
	}

	tbody tr:hover {
		background: var(--surface-2);
	}

	.num {
		text-align: right;
		font-variant-numeric: tabular-nums;
	}
</style>
