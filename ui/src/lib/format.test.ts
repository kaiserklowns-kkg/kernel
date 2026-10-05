import { describe, expect, test } from 'bun:test';
import {
	decision,
	kind,
	memoryTone,
	normalizeCode,
	plausibleCode,
	runtime,
	size,
	uptime,
	usedPercent,
	webAppURL
} from './format';

describe('uptime', () => {
	test('scales to the largest useful unit', () => {
		expect(uptime(0)).toBe('0 s');
		expect(uptime(42.9)).toBe('42 s');
		expect(uptime(300)).toBe('5 min');
		expect(uptime(7200)).toBe('2 h');
		expect(uptime(7500)).toBe('2 h 5 min');
		expect(uptime(86400 * 3 + 3600 * 4)).toBe('3 d 4 h');
		expect(uptime(-5)).toBe('0 s');
	});
});

describe('sizes', () => {
	test('KiB, MiB and GiB', () => {
		expect(size(512)).toBe('512 KiB');
		expect(size(1536)).toBe('1.5 MiB');
		expect(size(200 * 1024)).toBe('200 MiB');
		expect(size(3 * 1024 * 1024)).toBe('3.0 GiB');
	});
	test('memory in use', () => {
		expect(usedPercent({ totalMiB: 200, freeMiB: 50, kernelHeapKiB: 0 })).toBe(75);
		expect(usedPercent({ totalMiB: 0, freeMiB: 0, kernelHeapKiB: 0 })).toBe(0);
		expect(usedPercent({ totalMiB: 100, freeMiB: 200, kernelHeapKiB: 0 })).toBe(0);
		expect(memoryTone(50)).toBe('accent');
		expect(memoryTone(92)).toBe('warning');
		expect(memoryTone(99)).toBe('error');
	});
});

describe('words', () => {
	test('decisions have a label and a tone', () => {
		expect(decision('allowed')).toEqual({ label: 'Allowed', tone: 'success' });
		expect(decision('undecided').tone).toBe('warning');
	});
	test('kinds and runtimes', () => {
		expect(kind('app')).toBe('App');
		expect(kind('service (enabled)')).toBe('Service · enabled');
		expect(kind('')).toBe('');
		expect(runtime('wasm')).toBe('Go (WebAssembly)');
		expect(runtime('native')).toBe('Native');
	});
	test('pairing codes', () => {
		expect(normalizeCode(' 0123 4567-89AB ')).toBe('0123456789ab');
		expect(plausibleCode('0123456789abcdef0123456789abcdef')).toBe(true);
		expect(plausibleCode('0123')).toBe(false);
		expect(plausibleCode('g123456789abcdef0123456789abcdef')).toBe(false);
	});
});

describe('web apps', () => {
	test('open on their own port, beside the page', () => {
		expect(webAppURL('app.example.notes', { protocol: 'http:', hostname: '10.0.2.15' })).toBe(
			'http://10.0.2.15:8081/app.example.notes/'
		);
		expect(runtime('web')).toBe('Web (SvelteKit)');
	});
});
