<script lang="ts">
	import { resolve } from '$app/paths';
	import Logo from './Logo.svelte';
	import { NAME } from './site.js';

	/** Set once this browser has given an email, so later downloads skip the dialog. */
	const GIVEN = 'download-email';
	/** How long the download waits for the email to be saved before it starts anyway. */
	const SAVE_TIMEOUT = 3000;

	let dialog = $state<HTMLDialogElement>();
	let sending = $state(false);
	let invalid = $state(false);
	let done = $state(false);

	/** Opens the dialog in place of a download link's navigation, unless an email was already given. */
	export function intercept(event: MouseEvent) {
		try {
			if (localStorage.getItem(GIVEN)) return;
		} catch {
			// Storage can be blocked; ask for the email again.
		}
		event.preventDefault();
		invalid = false;
		done = false;
		dialog?.showModal();
	}

	async function submit(event: SubmitEvent & { currentTarget: HTMLFormElement }) {
		event.preventDefault();
		const form = event.currentTarget;
		sending = true;
		invalid = false;
		const controller = new AbortController();
		const timer = setTimeout(() => controller.abort(), SAVE_TIMEOUT);
		const res = await fetch(form.action, {
			method: 'POST',
			body: new FormData(form),
			signal: controller.signal
		}).catch(() => undefined);
		clearTimeout(timer);
		sending = false;
		if (res?.status === 400) {
			invalid = true;
			return;
		}
		// A failed or slow save still downloads. The dialog asks again next time.
		if (res?.ok) {
			try {
				localStorage.setItem(GIVEN, '1');
			} catch {
				// Storage can be blocked.
			}
		}
		done = true;
		location.href = resolve('/download');
	}

	function close() {
		dialog?.close();
	}
</script>

<dialog
	bind:this={dialog}
	aria-labelledby="download-title"
	onclick={(event) => event.target === dialog && close()}
>
	<div class="body">
		<Logo size={48} />
		{#if done}
			<h2 id="download-title">{NAME} is downloading</h2>
			<p>
				Open the disk image and drag {NAME} into Applications. If nothing downloaded,
				<a href={resolve('/download')}>try again</a>.
			</p>
			<button type="button" class="primary" onclick={close} {@attach (node) => node.focus()}>
				Done
			</button>
		{:else}
			<h2 id="download-title">Download {NAME}</h2>
			<p>
				Enter your email and the download starts right away. We’ll only use it to tell you about new
				versions.
			</p>
			<form method="POST" action={resolve('/download')} onsubmit={submit}>
				<input
					type="email"
					name="email"
					required
					autocomplete="email"
					placeholder="you@example.com"
					aria-label="Email"
					aria-invalid={invalid}
					aria-describedby={invalid ? 'download-error' : undefined}
				/>
				{#if invalid}
					<p id="download-error" class="error">That email doesn’t look right.</p>
				{/if}
				<button class="primary" disabled={sending}>Download for macOS</button>
			</form>
		{/if}
	</div>
	<button type="button" class="close" aria-label="Close" onclick={close}>
		<svg viewBox="0 0 12 12" aria-hidden="true">
			<path d="M2.5 2.5l7 7M9.5 2.5l-7 7" />
		</svg>
	</button>
</dialog>

<style>
	dialog {
		width: min(400px, calc(100vw - 32px));
		padding: 0;
		border: 0;
		border-radius: 22px;
		background: var(--card);
		box-shadow:
			var(--card-edge),
			0 30px 80px rgba(0, 0, 0, 0.55);
		color: var(--fg);
	}

	:global(:root[data-theme='light']) dialog {
		box-shadow:
			var(--card-edge),
			0 30px 80px rgba(0, 0, 0, 0.18);
	}

	dialog::backdrop {
		background: rgba(0, 0, 0, 0.5);
		backdrop-filter: blur(8px);
	}

	:global(:root[data-theme='light']) dialog::backdrop {
		background: rgba(0, 0, 0, 0.22);
	}

	.body {
		--error: #ff453a;
		display: flex;
		flex-direction: column;
		align-items: center;
		padding: 36px 28px 28px;
		text-align: center;
	}

	:global(:root[data-theme='light']) .body {
		--error: #d72828;
	}

	h2 {
		margin: 18px 0 0;
		font: 600 20px/26px var(--display);
		letter-spacing: -0.02em;
	}

	p {
		margin: 8px 0 0;
		color: var(--muted);
		font-size: 14px;
		line-height: 21px;
		text-wrap: pretty;
	}

	form {
		display: flex;
		flex-direction: column;
		gap: 10px;
		align-self: stretch;
		margin-top: 22px;
	}

	input {
		height: 44px;
		padding: 0 14px;
		border: 0;
		border-radius: 12px;
		background: color-mix(in srgb, var(--fg) 6%, transparent);
		box-shadow: inset 0 0 0 1px var(--line);
		color: var(--fg);
		font: 15px var(--sans);
	}

	input::placeholder {
		color: var(--muted);
	}

	input:focus-visible {
		outline-offset: 0;
	}

	input[aria-invalid='true'] {
		box-shadow: inset 0 0 0 1px var(--error);
	}

	.error {
		margin: 0;
		color: var(--error);
		font-size: 13px;
		line-height: 18px;
		text-align: left;
	}

	.primary {
		align-self: stretch;
		height: 44px;
		border: 0;
		border-radius: 999px;
		background: var(--solid);
		color: var(--on-solid);
		font: 600 15px var(--sans);
		cursor: pointer;
		transition: background-color 0.2s;
	}

	.primary:hover {
		background: var(--solid-hover);
	}

	.primary:disabled {
		opacity: 0.6;
		cursor: progress;
	}

	.body > .primary {
		margin-top: 22px;
	}

	.close {
		position: absolute;
		top: 12px;
		right: 12px;
		display: grid;
		place-items: center;
		width: 30px;
		height: 30px;
		padding: 0;
		border: 0;
		border-radius: 999px;
		background: none;
		color: var(--muted);
		cursor: pointer;
		transition:
			color 0.15s,
			background-color 0.15s;
	}

	.close:hover {
		background: color-mix(in srgb, var(--fg) 8%, transparent);
		color: var(--fg);
	}

	.close svg {
		width: 12px;
		height: 12px;
		fill: none;
		stroke: currentColor;
		stroke-width: 1.5;
		stroke-linecap: round;
	}

	@media (prefers-reduced-motion: no-preference) {
		dialog[open] {
			animation: pop 0.3s cubic-bezier(0.2, 0.7, 0.2, 1);
		}

		dialog[open]::backdrop {
			animation: fade 0.3s ease-out;
		}
	}

	@keyframes pop {
		from {
			opacity: 0;
			scale: 0.94;
		}
	}

	@keyframes fade {
		from {
			opacity: 0;
		}
	}
</style>
