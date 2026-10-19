<script lang="ts">
	let { size }: { size: number } = $props();
	const id = $props.id();
</script>

<svg width={size} height={size} viewBox="0 0 100 100" fill="none" aria-hidden="true">
	<defs>
		<linearGradient id="{id}-bg" x1="0" y1="0" x2="0.35" y2="1">
			<stop offset="0" stop-color="#303036" />
			<stop offset="1" stop-color="#09090B" />
		</linearGradient>
		<radialGradient id="{id}-glow" cx="0.28" cy="0.06" r="0.95">
			<stop offset="0" stop-color="#6E9BFF" stop-opacity="0.32" />
			<stop offset="1" stop-color="#6E9BFF" stop-opacity="0" />
		</radialGradient>
		<linearGradient id="{id}-sheen" x1="0" y1="0" x2="0" y2="1">
			<stop offset="0" stop-color="#FFFFFF" stop-opacity="0.16" />
			<stop offset="1" stop-color="#FFFFFF" stop-opacity="0" />
		</linearGradient>
		<linearGradient id="{id}-glass" x1="0" y1="0" x2="0" y2="1">
			<stop offset="0" stop-color="#FFFFFF" />
			<stop offset="1" stop-color="#FFFFFF" stop-opacity="0.74" />
		</linearGradient>
		<linearGradient id="{id}-rim" x1="0" y1="0" x2="0" y2="1">
			<stop offset="0" stop-color="#FFFFFF" stop-opacity="0.42" />
			<stop offset="0.45" stop-color="#FFFFFF" stop-opacity="0.06" />
			<stop offset="1" stop-color="#FFFFFF" stop-opacity="0.16" />
		</linearGradient>
	</defs>
	<rect width="100" height="100" rx="22.5" fill="url(#{id}-bg)" />
	<rect width="100" height="100" rx="22.5" fill="url(#{id}-glow)" />
	<path d="M0 22.5A22.5 22.5 0 0 1 22.5 0h55A22.5 22.5 0 0 1 100 22.5V46H0Z" fill="url(#{id}-sheen)" />
	<g transform="translate(0 2.6)" fill="#000" opacity="0.5">
		<rect x="24.8" y="44" width="4.4" height="39" rx="2.2" />
		<rect x="18" y="53" width="18" height="23" rx="4" />
		<rect x="47.8" y="30" width="4.4" height="40" rx="2.2" />
		<rect x="41" y="37" width="18" height="25" rx="4" />
		<rect x="70.8" y="16" width="4.4" height="40" rx="2.2" />
		<rect x="64" y="23" width="18" height="25" rx="4" />
	</g>
	<g fill="url(#{id}-glass)">
		<rect x="24.8" y="44" width="4.4" height="39" rx="2.2" />
		<rect x="18" y="53" width="18" height="23" rx="4" />
		<rect x="47.8" y="30" width="4.4" height="40" rx="2.2" />
		<rect x="41" y="37" width="18" height="25" rx="4" />
		<rect x="70.8" y="16" width="4.4" height="40" rx="2.2" />
		<rect x="64" y="23" width="18" height="25" rx="4" />
	</g>
	<rect x="21" y="54.3" width="12" height="1.2" rx="0.6" fill="#FFF" />
	<rect x="44" y="38.3" width="12" height="1.2" rx="0.6" fill="#FFF" />
	<rect x="67" y="24.3" width="12" height="1.2" rx="0.6" fill="#FFF" />
	<rect x="0.5" y="0.5" width="99" height="99" rx="22" stroke="url(#{id}-rim)" />
</svg>
