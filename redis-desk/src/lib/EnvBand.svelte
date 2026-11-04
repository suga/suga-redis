<script lang="ts">
	/**
	 * A barra do topo. É o único elemento forte da tela: enquanto ela
	 * estiver vermelha, quem está olhando sabe que um erro ali é um erro em
	 * produção. Por isso ocupa a largura inteira e não rola junto com o
	 * conteúdo.
	 */
	import { envChrome } from "$lib/env";
	import type { Host } from "$lib/api";

	interface Props {
		host: Host | null;
		/** Tempo de resposta do último teste de conexão, em milissegundos. */
		latencyMs?: number | null;
	}

	let { host, latencyMs = null }: Props = $props();

	let chrome = $derived(host ? envChrome(host.env) : null);
</script>

{#if host && chrome}
	<header
		class="band"
		data-testid="env-band"
		data-env={host.env}
		style="--env-band: {chrome.band}; --env-border: {chrome.border}"
	>
		<span class="stamp">{chrome.label}</span>
		<span class="name">{host.name}</span>
		<span class="addr mono">{host.addr}:{host.port}/{host.db}</span>
		{#if latencyMs !== null}
			<span class="latency mono">{latencyMs} ms</span>
		{/if}
	</header>
{:else}
	<header class="band band--empty" data-testid="env-band">
		<span class="name">Nenhum host aberto</span>
	</header>
{/if}

<style>
	.band {
		display: flex;
		align-items: baseline;
		gap: 1rem;
		padding: 0.55rem 1rem;
		background: var(--env-band);
		border-bottom: 3px solid var(--env-border);
		color: #f6f8fa;
	}

	.band--empty {
		background: var(--panel-dark);
		border-bottom: 3px solid var(--rule);
		color: var(--text-faint);
	}

	/* O âmbar da homologação é claro demais para texto branco. */
	.band[data-env="homolog"] {
		color: #1c2024;
	}

	.stamp {
		font-weight: 700;
		letter-spacing: 0.14em;
		font-size: 0.95rem;
	}

	.name {
		font-weight: 500;
		font-size: 1.05rem;
	}

	.addr {
		opacity: 0.82;
	}

	.latency {
		margin-left: auto;
		opacity: 0.82;
	}
</style>
