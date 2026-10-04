// Whether this browser is paired, shared by every page. A 401 from any
// request (the code was wrong, or `ui unpair` revoked it) sends the app
// back to the pairing screen.

import { ApiError, api } from './api';

type State = 'checking' | 'unpaired' | 'paired' | 'unreachable';

class PairingState {
	state = $state<State>('checking');
	/** Whether Oceans has a pairing at all (else `ui pair` was not run). */
	systemPaired = $state(false);
	message = $state('');

	async check(): Promise<void> {
		try {
			const session = await api.session();
			this.systemPaired = session.paired;
			this.state = session.authenticated ? 'paired' : 'unpaired';
			this.message = '';
		} catch (error) {
			this.state = 'unreachable';
			this.message = error instanceof Error ? error.message : String(error);
		}
	}

	async login(code: string): Promise<void> {
		await api.login(code);
		await this.check();
	}

	async logout(): Promise<void> {
		try {
			await api.logout();
		} finally {
			await this.check();
		}
	}

	/**
	 * Runs a request; a 401 means the pairing is gone, so the app returns
	 * to the pairing screen. Other errors are the caller's to show.
	 */
	async guard<T>(request: () => Promise<T>): Promise<T> {
		try {
			return await request();
		} catch (error) {
			if (error instanceof ApiError && error.unauthorized) {
				this.state = 'unpaired';
				this.message = error.message;
			}
			throw error;
		}
	}
}

export const pairing = new PairingState();

/** The text to show for a failure. */
export function describe(error: unknown): string {
	if (error instanceof Error) return error.message;
	return String(error);
}
