// Presentation helpers: plain functions, tested with `bun test`.

import type { Decision, Memory } from './api';

export type Tone = 'neutral' | 'accent' | 'success' | 'warning' | 'error' | 'info';

/** `0 s`, `42 s`, `5 min`, `2 h 5 min`, `3 d 4 h`. */
export function uptime(seconds: number): string {
	const s = Math.max(0, Math.floor(seconds));
	const days = Math.floor(s / 86400);
	const hours = Math.floor((s % 86400) / 3600);
	const minutes = Math.floor((s % 3600) / 60);
	if (days > 0) return hours > 0 ? `${days} d ${hours} h` : `${days} d`;
	if (hours > 0) return minutes > 0 ? `${hours} h ${minutes} min` : `${hours} h`;
	if (minutes > 0) return `${minutes} min`;
	return `${s} s`;
}

/** KiB as KiB, MiB or GiB with one decimal where it helps. */
export function size(kib: number): string {
	if (kib < 1024) return `${Math.round(kib)} KiB`;
	const mib = kib / 1024;
	if (mib < 1024) return `${mib < 10 ? mib.toFixed(1) : Math.round(mib)} MiB`;
	const gib = mib / 1024;
	return `${gib < 10 ? gib.toFixed(1) : Math.round(gib)} GiB`;
}

/** The share of memory in use, 0–100 (0 when the total is unknown). */
export function usedPercent(memory: Memory): number {
	if (memory.totalMiB <= 0) return 0;
	const used = Math.max(0, memory.totalMiB - memory.freeMiB);
	return Math.min(100, Math.round((used / memory.totalMiB) * 100));
}

/** Memory use above 90 % is a warning; above 97 % an error. */
export function memoryTone(percent: number): Tone {
	if (percent >= 97) return 'error';
	if (percent >= 90) return 'warning';
	return 'accent';
}

const decisionWords: Record<Decision, { label: string; tone: Tone }> = {
	automatic: { label: 'Automatic', tone: 'neutral' },
	allowed: { label: 'Allowed', tone: 'success' },
	denied: { label: 'Denied', tone: 'error' },
	undecided: { label: 'Not decided', tone: 'warning' }
};

export function decision(d: Decision): { label: string; tone: Tone } {
	return decisionWords[d];
}

/** `service (enabled)` → `Service · enabled`; `app` → `App`. */
export function kind(text: string): string {
	const match = /^(\w+)(?:\s*\((.+)\))?$/.exec(text.trim());
	if (!match || match[1] === undefined) return text;
	const word = match[1].charAt(0).toUpperCase() + match[1].slice(1);
	return match[2] ? `${word} · ${match[2]}` : word;
}

export function runtime(text: string): string {
	if (text === 'wasm') return 'Go (WebAssembly)';
	if (text === 'native') return 'Native';
	if (text === 'web') return 'Web (SvelteKit)';
	return text;
}

/** The port web apps are served from (ADR-0064): their own origin. */
export const webAppPort = 8081;

/** Where a web app opens, beside this page's host. */
export function webAppURL(id: string, location: { protocol: string; hostname: string }): string {
	return `${location.protocol}//${location.hostname}:${webAppPort}/${encodeURIComponent(id)}/`;
}

/** A pairing code as typed: hex digits, spaces and dashes ignored. */
export function normalizeCode(input: string): string {
	return input.replace(/[\s-]/g, '').toLowerCase();
}

export function plausibleCode(code: string): boolean {
	return /^[0-9a-f]{32,128}$/.test(code);
}
