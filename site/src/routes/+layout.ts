import { dev } from '$app/env';
import { inject } from '@vercel/analytics';

// `@vercel/analytics/sveltekit` subscribes to `$app/stores`, which throws in SvelteKit 3.
inject({ mode: dev ? 'development' : 'production', framework: 'sveltekit' });
