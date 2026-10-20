import { error, redirect } from '@sveltejs/kit';
import { Redis } from '@upstash/redis';
import { KV_REST_API_TOKEN, KV_REST_API_URL } from '$app/env/private';
import { DOWNLOAD_URL } from '#lib/site.js';
import type { RequestHandler } from './$types';

/** The HTML spec's rule for `<input type="email">`, so the server accepts what the form accepts. */
const EMAIL =
	/^[a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)*$/;

export const GET: RequestHandler = () => {
	redirect(302, DOWNLOAD_URL, { external: true });
};

/**
 * Saves the email from the download dialog to a sorted set scored by when the address was first
 * given. The dialog starts the download with `GET` once this answers.
 */
export const POST: RequestHandler = async ({ request }) => {
	const email = String((await request.formData()).get('email') ?? '')
		.trim()
		.toLowerCase();
	if (email.length > 254 || !EMAIL.test(email)) error(400, 'Invalid email address');
	if (!KV_REST_API_URL || !KV_REST_API_TOKEN) {
		throw new Error('KV_REST_API_URL and KV_REST_API_TOKEN must be set to save download emails');
	}
	const redis = new Redis({ url: KV_REST_API_URL, token: KV_REST_API_TOKEN });
	await redis.zadd('emails', { nx: true }, { score: Date.now(), member: email });
	return new Response(null, { status: 204 });
};
