<script lang="ts">
	/**
	 * A coluna de hosts.
	 *
	 * Cada linha carrega uma marca na cor do seu ambiente, então a coluna
	 * inteira é lida de relance como um índice colorido: dá para ver quantos
	 * hosts de produção existem sem ler nome nenhum.
	 */
	import { envChrome } from "$lib/env";
	import { errorMessage, removeHost, testHost, type Host } from "$lib/api";

	interface Props {
		hosts: Host[];
		selectedId: string | null;
		onSelect: (host: Host) => void;
		onNew: () => void;
		onEdit: (host: Host) => void;
		/** Chamado depois de remover, para a tela recarregar a lista. */
		onChanged: () => void;
	}

	let { hosts, selectedId, onSelect, onNew, onEdit, onChanged }: Props = $props();

	/** Resultado do último teste por host: a latência, ou a falha. */
	let testes = $state<Record<string, { ms: number } | { erro: string }>>({});
	let testando = $state<string | null>(null);

	async function testar(host: Host) {
		testando = host.id;
		try {
			testes[host.id] = { ms: await testHost(host.id) };
		} catch (err) {
			testes[host.id] = { erro: errorMessage(err) };
		} finally {
			testando = null;
		}
	}

	async function remover(host: Host) {
		try {
			await removeHost(host.id);
			onChanged();
		} catch (err) {
			testes[host.id] = { erro: errorMessage(err) };
		}
	}
</script>

<nav>
	<div class="topo">
		<h2>Hosts</h2>
		<button onclick={onNew}>Novo host</button>
	</div>

	{#if hosts.length === 0}
		<p class="vazio">Cadastre um host para começar.</p>
	{:else}
		<ul>
			{#each hosts as host (host.id)}
				{@const resultado = testes[host.id]}
				<li
					class="linha"
					class:selecionada={host.id === selectedId}
					style="--marca: {envChrome(host.env).band}"
				>
					<button class="abrir" onclick={() => onSelect(host)}>
						<span class="nome">{host.name}</span>
						<span class="endereco mono">{host.addr}:{host.port}/{host.db}</span>
					</button>

					<div class="acoes">
						<button onclick={() => testar(host)} disabled={testando === host.id}>
							{testando === host.id ? "Testando" : "Testar"}
						</button>
						<button onclick={() => onEdit(host)}>Editar</button>
						<button class="danger" onclick={() => remover(host)}>Remover</button>
					</div>

					{#if resultado && "ms" in resultado}
						<p class="resultado mono">Respondeu em {resultado.ms} ms</p>
					{:else if resultado}
						<p class="resultado falha" role="alert">{resultado.erro}</p>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
</nav>

<style>
	nav {
		display: flex;
		flex-direction: column;
		height: 100%;
		background: var(--panel);
		border-right: 1px solid var(--rule);
	}

	.topo {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		padding: 0.7rem 0.8rem;
		border-bottom: 1px solid var(--rule);
	}

	h2 {
		margin: 0;
		font-size: 1.05rem;
		font-weight: 600;
	}

	.vazio {
		margin: 0;
		padding: 1rem 0.8rem;
		color: var(--text-faint);
	}

	ul {
		list-style: none;
		margin: 0;
		padding: 0;
		overflow-y: auto;
	}

	/* A marca do ambiente: a informação que precisa sobreviver a uma olhada
	   rápida na coluna inteira. */
	.linha {
		border-left: 4px solid var(--marca);
		border-bottom: 1px solid var(--rule);
		padding: 0.5rem 0.8rem 0.6rem;
	}

	.linha.selecionada {
		background: var(--panel-dark);
	}

	.abrir {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.1rem;
		width: 100%;
		padding: 0;
		border: none;
		text-align: left;
	}

	.abrir:hover .nome {
		color: var(--action);
	}

	.nome {
		font-weight: 500;
	}

	.endereco {
		color: var(--text-dim);
	}

	.acoes {
		display: flex;
		gap: 0.3rem;
		margin-top: 0.45rem;
	}

	.acoes button {
		padding: 0.15rem 0.45rem;
		font-size: 0.88rem;
		color: var(--text-dim);
	}

	.resultado {
		margin: 0.45rem 0 0;
		color: var(--text-dim);
		font-size: 0.9rem;
	}

	.falha {
		color: #e49a94;
	}
</style>
