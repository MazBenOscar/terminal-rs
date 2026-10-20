import { defineEnvVars } from '@sveltejs/kit/env';

/** Unset, the site still builds and serves downloads, but `POST /download` can't save emails. */
const optional = (value: string | undefined) => value || undefined;

export const variables = defineEnvVars({
	KV_REST_API_URL: {
		schema: optional,
		description: 'REST URL of the Upstash Redis database that stores download emails'
	},
	KV_REST_API_TOKEN: {
		schema: optional,
		description: 'Read-write token for the Upstash Redis database that stores download emails'
	}
});
