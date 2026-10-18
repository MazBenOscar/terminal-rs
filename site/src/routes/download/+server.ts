import { redirect } from '@sveltejs/kit';
import { DOWNLOAD_URL } from '#lib/site.js';
import type { RequestHandler } from './$types';

export const GET: RequestHandler = () => {
	redirect(302, DOWNLOAD_URL, { external: true });
};
