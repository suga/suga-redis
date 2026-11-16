<script lang="ts">
	import { idioma, t } from "$lib/i18n.svelte";
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

	/**
	 * De quanto em quanto tempo a lista se relê sozinha.
	 *
	 * Lida só com o anel em memória: uma trava e uma cópia de no máximo
	 * 1000 entradas, sem rede e sem disco. Barato o bastante para pagar
	 * enquanto a aba estiver aberta, e só enquanto estiver. Três segundos é
	 * o mesmo intervalo que a fila de aprovações já usa em `+page.svelte`.
	 */
	const INTERVALO_MS = 3000;

	let entradas = $state<LogEntry[]>([]);
	// Começa true: o instante antes do onMount resolver é "ainda não lido",
	// não "lido e vazio" — os dois não podem parecer a mesma tela por
	// coincidência de tempo.
	let carregando = $state(true);
	let erro = $state("");
	/** Quando a lista na tela foi lida. `0` = nunca. */
	let lidoEm = $state(0);

	async function carregar() {
		carregando = true;
		try {
			entradas = await logs();
			erro = "";
			lidoEm = Date.now();
		} catch (err) {
			// A lista que já está na tela NÃO é apagada: uma releitura que
			// falha não desfaz o que foi lido antes, e apagar deixaria a
			// aba em branco justamente quando algo está dando errado.
			erro = `Não foi possível ler o log: ${errorMessage(err)}`;
		} finally {
			carregando = false;
		}
	}

	// Lê ao montar — inclusive quando a pessoa volta para esta aba depois de
	// olhar outra, já que a troca de aba em Workspace.svelte desmonta e
	// remonta o componente — e relê sozinha enquanto estiver aberta.
	//
	// Sem a releitura, ficar olhando esta aba durante uma varredura ou uma
	// queda de conexão mostrava uma lista congelada, sem nada na tela
	// dizendo que estava congelada: quem acompanha um host de produção se
	// comportando mal concluiria que nada está sendo registrado. O botão
	// "Atualizar" continua existindo ao lado da hora da última leitura,
	// para quem quiser perguntar agora e para que a tela diga, em palavras,
	// de quando é o que está vendo.
	onMount(() => {
		carregar();
		const relogio = setInterval(carregar, INTERVALO_MS);
		return () => clearInterval(relogio);
	});

	function hora(ms: number): string {
		const quando = new Date(ms);
		const relogio = quando.toLocaleTimeString(idioma(), { hour12: false });
		// Este é um aplicativo de mesa que fica dias aberto: sem a data, a
		// entrada de segunda-feira se lê como "agora há pouco".
		if (quando.toDateString() === new Date().toDateString()) {
			return relogio;
		}
		const dia = quando.toLocaleDateString(idioma(), {
			day: "2-digit",
			month: "2-digit",
		});
		return `${dia} ${relogio}`;
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
	<div class="topo">
		<!-- A hora da última leitura é parte da honestidade da aba: sem ela, a
		     lista na tela não diz de quando é — e uma lista parada parece
		     uma lista vazia de acontecimentos. -->
		<p class="aviso">
			{#if lidoEm > 0}
				{t("log.atualizadoAs", { hora: hora(lidoEm) })}
			{:else if carregando}
				{t("log.lendo")}
			{:else}
				{t("log.semLeitura")}
			{/if}
		</p>
		<button onclick={carregar} disabled={carregando}>{t("acao.recarregar")}</button>
	</div>

	{#if erro}
		<p class="erro" role="alert">{erro}</p>
	{/if}

	{#if entradas.length === 0}
		{#if lidoEm > 0 && !erro}
			<p class="vazio" data-testid="vazio">
				{t("log.vazio")}
			</p>
		{/if}
	{:else}
		<ul>
			<!-- Chave pelo índice, de propósito. Qualquer chave montada com o
			     CONTEÚDO da entrada é falsa: uma falha de "Conectar" empurra
			     duas linhas — a do pool e a da fachada — no mesmo
			     milissegundo, com o mesmo host e a mesma operação, diferindo
			     só no tipo. O Svelte 5 derruba o bloco inteiro ao ver chave
			     repetida, e derrubava justamente a aba que a pessoa abriu
			     para descobrir por que a conexão falhou. A lista é um retrato
			     imutável, trocado por inteiro a cada leitura, então o índice
			     identifica a posição tão bem quanto qualquer outra coisa e
			     não depende do que está escrito na linha. -->
			{#each entradas as entrada, i (i)}
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

	.topo {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 0.6rem;
		margin-bottom: 0.7rem;
	}

	.topo button {
		padding: 0.12rem 0.45rem;
		font-size: 0.88rem;
		color: var(--text-dim);
		white-space: nowrap;
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

	.aviso {
		font-size: 0.88rem;
	}

	.erro {
		margin: 0 0 0.7rem;
		padding: 0.4rem 0.6rem;
		border: 1px solid #5e3a38;
		border-radius: var(--radius);
		background: #2e2326;
		color: #e49a94;
	}
</style>
