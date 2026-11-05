<script lang="ts">
	/**
	 * O log do aplicativo inteiro, mais recente primeiro: conexões, falhas e
	 * operações lentas de TODOS os hosts, não só o que está aberto.
	 *
	 * Existe por causa de um incidente: um host de produção subiu mostrando
	 * "nenhuma chave" e não havia como distinguir uma conexão que falhou de
	 * um banco genuinamente vazio. Por isso o estado vazio e o estado de erro
	 * de leitura precisam ser telas diferentes — um painel em branco nos dois
	 * casos reproduziria exatamente o defeito que esta aba existe para sanar.
	 *
	 * Lista, não tabela: a coluna onde isto vive é estreita e uma tarefa
	 * futura torna esta aba arrastável, então uma grade de colunas fixas
	 * quebraria o layout.
	 */
	import { errorMessage, logs, type LogEntry } from "$lib/api";
	import { onMount } from "svelte";

	let entradas = $state<LogEntry[]>([]);
	// Começa true: o instante antes do onMount resolver é "ainda não lido",
	// não "lido e vazio" — os dois não podem parecer a mesma tela por
	// coincidência de tempo.
	let carregando = $state(true);
	let erro = $state("");

	async function carregar() {
		carregando = true;
		erro = "";
		try {
			entradas = await logs();
		} catch (err) {
			entradas = [];
			erro = `Não foi possível ler o log: ${errorMessage(err)}`;
		} finally {
			carregando = false;
		}
	}

	// Recarrega sempre que a aba é montada — inclusive quando a pessoa volta
	// para ela depois de olhar outra, já que o componente é desmontado e
	// remontado pela troca de aba em Workspace.svelte.
	onMount(() => {
		carregar();
	});

	function hora(ms: number): string {
		return new Date(ms).toLocaleTimeString("pt-BR", { hour12: false });
	}

	function duracao(ms: number): string {
		if (ms < 1000) return `${ms} ms`;
		return `${(ms / 1000).toFixed(1)} s`;
	}

	const ROTULO: Record<LogEntry["kind"], string> = {
		connection: "conexão",
		failure: "falha",
		slow: "lento",
	};
</script>

<section>
	{#if erro}
		<p class="erro" role="alert">{erro}</p>
	{:else if carregando && entradas.length === 0}
		<p class="aviso">Lendo o log…</p>
	{:else if entradas.length === 0}
		<p class="vazio" data-testid="vazio">
			Nenhum evento registrado ainda. Conexões, falhas e operações lentas de
			qualquer host aparecem aqui assim que acontecerem.
		</p>
	{:else}
		<ul>
			{#each entradas as entrada (`${entrada.at_ms}-${entrada.host}-${entrada.operation}`)}
				<li data-testid="entrada">
					<div class="cabeca">
						<span class="tipo tipo--{entrada.kind}">{ROTULO[entrada.kind]}</span>
						<span class="hora mono">{hora(entrada.at_ms)}</span>
					</div>
					<div class="linha">
						<span class="host">{entrada.host}</span>
						{#if entrada.db !== null}
							<span class="banco" data-testid="banco">banco {entrada.db}</span>
						{/if}
						<span class="operacao">
							{entrada.operation}{entrada.duration_ms !== null
								? ` · ${duracao(entrada.duration_ms)}`
								: ""}
						</span>
					</div>
					{#if entrada.detail}
						<p class="detalhe">{entrada.detail}</p>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
</section>

<style>
	section {
		min-width: 0;
	}

	ul {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.7rem;
	}

	li {
		padding-bottom: 0.7rem;
		border-bottom: 1px solid var(--rule);
	}

	.cabeca {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 1rem;
	}

	.tipo {
		font-size: 0.82rem;
		font-weight: 500;
	}

	/* Verde, âmbar e cinza — nunca vermelho, que neste app só quer dizer
	   produção. Os tons aqui são deliberadamente dessaturados e mais claros
	   que --dev (#4e7a4a) e --homolog (#c98a20): um verde ou âmbar na mesma
	   saturação dos tokens de ambiente seria quase gêmeo deles na tela, e
	   ambiente é a única cor forte que este app se permite ter. */
	.tipo--connection {
		color: #87a18e;
	}

	.tipo--failure {
		color: #bbac81;
	}

	.tipo--slow {
		color: var(--text-faint);
	}

	.hora {
		color: var(--text-faint);
		font-size: 0.85rem;
	}

	.linha {
		display: flex;
		flex-wrap: wrap;
		gap: 0.6rem;
		margin-top: 0.2rem;
	}

	.host {
		color: var(--text-dim);
		word-break: break-word;
	}

	.banco {
		color: var(--text-dim);
	}

	.operacao {
		color: var(--text);
		word-break: break-word;
	}

	.detalhe {
		margin: 0.35rem 0 0;
		color: var(--text-faint);
		font-size: 0.88rem;
		word-break: break-word;
	}

	.aviso,
	.vazio {
		margin: 0;
		color: var(--text-faint);
	}

	.erro {
		margin: 0;
		padding: 0.4rem 0.6rem;
		border: 1px solid #5e3a38;
		border-radius: var(--radius);
		background: #2e2326;
		color: #e49a94;
	}
</style>
