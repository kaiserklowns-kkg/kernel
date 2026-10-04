<script lang="ts">
	import type { Tone } from '$lib/format';

	let { value, label, tone = 'accent' }: { value: number; label: string; tone?: Tone } = $props();

	const clamped = $derived(Math.max(0, Math.min(100, value)));
</script>

<!-- The width is set through the CSSOM (style:), which the page's
     Content-Security-Policy allows; inline style attributes it does not. -->
<div
	class="meter {tone}"
	role="meter"
	aria-label={label}
	aria-valuemin={0}
	aria-valuemax={100}
	aria-valuenow={clamped}
	aria-valuetext="{clamped}%"
>
	<div class="fill" style:width="{clamped}%"></div>
</div>

<style>
	.meter {
		height: 8px;
		border-radius: 999px;
		background: var(--surface-3);
		overflow: hidden;
	}

	.fill {
		height: 100%;
		border-radius: inherit;
		background: var(--accent);
		transition: width 400ms ease;
	}

	.warning .fill {
		background: var(--warning);
	}

	.error .fill {
		background: var(--error);
	}

	.success .fill {
		background: var(--success);
	}
</style>
