<script lang="ts">
	import '../app.css';
	import { asset } from '$app/paths';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();
</script>

<svelte:head>
	<link rel="icon" href={asset('favicon.svg')} type="image/svg+xml" />
	<link rel="icon" href={asset('favicon.png')} type="image/png" sizes="64x64" />
	<link rel="apple-touch-icon" href={asset('apple-touch-icon.png')} />
</svelte:head>

{@render children()}
