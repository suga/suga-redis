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
				{@const chrome = envChrome(host.env)}
				<li
					class="linha"
					class:selecionada={host.id === selectedId}
					style="--marca: {chrome.band}"
					data-env={host.env}
				>
					<div class="cabeca">
						<button class="abrir" onclick={() => onSelect(host)}>
							{host.name}
						</button>
						<span class="flag" data-testid="flag-ambiente">{chrome.label}</span>
					</div>

					<!-- Endereço de cluster gerenciado passa de 70 caracteres. Cortado
					     para caber na coluna; o texto inteiro fica no `title`, porque é
					     ele que distingue dois clusters do mesmo ambiente. -->
					<p
						class="endereco mono"
						data-testid="endereco"
						title="{host.addr}:{host.port}/{host.db}"
					>
						{host.addr}:{host.port}/{host.db}
					</p>

					<div class="acoes">
						<button class="conectar" onclick={() => onSelect(host)}>Conectar</button>
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
		min-width: 0;
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
		min-width: 0;
		overflow-y: auto;
	}

	/* A marca do ambiente: a informação que precisa sobreviver a uma olhada
	   rápida na coluna inteira. */
	.linha {
		min-width: 0;
		border-left: 4px solid var(--marca);
		border-bottom: 1px solid var(--rule);
		padding: 0.5rem 0.8rem 0.6rem;
	}

	.linha.selecionada {
		background: var(--panel-dark);
	}

	/* `min-width: 0` em toda a cadeia: sem isso um item de flex se recusa a
	   encolher abaixo do conteúdo, e um endereço de 70 caracteres sem espaço
	   nenhum empurra a coluna inteira, criando a barra de rolagem horizontal. */
	.cabeca {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 0.5rem;
		min-width: 0;
	}

	.abrir {
		min-width: 0;
		padding: 0;
		border: none;
		text-align: left;
		font-weight: 500;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.abrir:hover {
		color: var(--action);
	}

	/* A flag diz em palavras o que a marca colorida da borda diz em cor.
	   Quem não decorou o código de cores ainda assim lê onde está. */
	.flag {
		flex: none;
		padding: 0.05rem 0.35rem;
		border-radius: var(--radius);
		background: var(--marca);
		color: #f6f8fa;
		font-size: 0.7rem;
		font-weight: 700;
		letter-spacing: 0.08em;
	}

	/* O âmbar da homologação é claro demais para texto branco. */
	.linha[data-env="homolog"] .flag {
		color: #1c2024;
	}

	.endereco {
		margin: 0.1rem 0 0;
		color: var(--text-dim);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.acoes {
		display: flex;
		flex-wrap: wrap;
		gap: 0.3rem;
		margin-top: 0.45rem;
	}

	/* Ação principal da linha, sem usar o âmbar: uma coluna com dez hosts
	   teria dez botões gritando, e o âmbar deixaria de significar "a ação
	   desta tela". */
	.acoes .conectar {
		color: var(--text);
		border-color: var(--text-faint);
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
