<script lang="ts">
	/**
	 * A área de trabalho de um host aberto: escolha do banco, árvore de
	 * chaves à esquerda, painel do valor à direita.
	 */
	import ConfirmDialog from "$lib/ConfirmDialog.svelte";
	import Divisor from "$lib/Divisor.svelte";
	import KeyEditor from "$lib/KeyEditor.svelte";
	import KeyTree from "$lib/KeyTree.svelte";
	import HistoryPanel from "$lib/HistoryPanel.svelte";
	import LogPanel from "$lib/LogPanel.svelte";
	import NewKeyDialog from "$lib/NewKeyDialog.svelte";
	import SearchPanel from "$lib/SearchPanel.svelte";
	import ValuePanel from "$lib/ValuePanel.svelte";
	import { haQuantoTempo, numero } from "$lib/formato";
	import {
		errorMessage,
		listDbs,
		onRescanProgress,
		onTreeProgress,
		scanTree,
		rescan,
		scanStatus,
		textBytes,
		writeValue,
		type Host,
		type ScanProgress,
		type Branch,
		type ScanSnapshot,
		type ScanReport,
		type WriteOp,
	} from "$lib/api";
	import { onMount, untrack } from "svelte";

	interface Props {
		host: Host;
		/**
		 * O motivo de uma falha de `connect` já observada por quem abriu
		 * este host — `null` quando a conexão deu certo, ou quando o
		 * workspace foi aberto sem passar por `connect` (clique no nome).
		 *
		 * Lido direto do prop, NUNCA copiado para um `$state` próprio: uma
		 * nova tentativa no MESMO host (falhou, e a pessoa clicou Conectar
		 * de novo) precisa refletir aqui sem exigir remontagem — e um
		 * `$state` inicializado a partir de um prop só captura o valor
		 * inicial, o mesmo problema que `docs/CONTEXTO.md` registra para
		 * `db` mais abaixo.
		 */
		falhaConexao: string | null;
	}

	let { host, falhaConexao }: Props = $props();

	/** Largura da coluna de chaves antes de qualquer arrasto — o mínimo que a coluna já tinha. */
	const LARGURA_CHAVES_PADRAO = 256; // 16rem
	let larguraChaves = $state(LARGURA_CHAVES_PADRAO);

	/**
	 * O banco parte do que o host traz cadastrado e passa a ser escolha da
	 * pessoa — recarregar a lista de hosts no fundo não pode puxá-la de
	 * volta para o banco do cadastro. Trocar de host é remontagem, feita com
	 * `{#key}` por quem usa este componente.
	 */
	let db = $state(untrack(() => host.db));
	let bancos = $state<[number, number][]>([]);
	let filtro = $state("");
	let selecionada = $state<string | null>(null);
	let relatorio = $state<ScanReport | null>(null);
	/**
	 * Falha ao perguntar o relatório de índice — distinta de `relatorio ===
	 * null`, que também é a resposta legítima de um banco nunca varrido.
	 * Sem isto, as duas viravam o mesmo "nunca varrido" na tela.
	 */
	let erroIndice = $state("");
	/**
	 * A pergunta ao índice está em voo — distinta de "nunca varrido", que é
	 * a resposta legítima quando a pergunta JÁ voltou e não há relatório.
	 * Sem isto, um host lento (a própria origem do incidente: 5679 ms só
	 * para um PING) mostrava "nunca varrido" enquanto ainda esperava a
	 * resposta — os mesmos pixels de uma pergunta respondida.
	 */
	let perguntandoIndice = $state(false);
	/**
	 * Sobe a cada nova pergunta ao índice. Uma troca rápida de banco dispara
	 * uma pergunta nova antes da anterior voltar; se a antiga responder
	 * depois, seu `.then`/`.catch` compara a geração que carregava consigo
	 * contra esta e desiste — sem isso, uma resposta atrasada de um banco
	 * que não é mais o aberto sobrescrevia o estado do banco atual.
	 */
	let geracaoIndice = 0;
	/**
	 * Falha ao listar os bancos — distinta de `bancos.length === 0`, que
	 * também é a resposta legítima de um host sem nenhum banco populado.
	 * Sem isto, o seletor ficava pelado nos dois casos.
	 */
	let erroBancos = $state("");
	let progresso = $state<ScanProgress | null>(null);
	/** Andamento da varredura que monta a árvore. */
	let lendo = $state<ScanSnapshot | null>(null);
	/** Os prefixos de primeiro nível, crescendo a cada retrato. */
	let raiz = $state<Branch[]>([]);
	/** Sobe a cada retrato: é o que manda a árvore reler os galhos abertos. */
	let versaoArvore = $state(0);
	let varrendo = $state(false);
	let erro = $state("");
	let criando = $state(false);
	/** Criação recusada em produção, esperando confirmação. */
	let criacaoPendente = $state<{ key: string; op: WriteOp; summary: string } | null>(null);
	/** Força o painel a reler a chave depois de uma escrita. */
	let revisao = $state(0);

	type Aba = "valor" | "busca" | "historico" | "logs";
	let aba = $state<Aba>("valor");
	const ABAS: { id: Aba; label: string }[] = [
		{ id: "valor", label: "Valor" },
		{ id: "busca", label: "Busca" },
		{ id: "historico", label: "Histórico" },
		{ id: "logs", label: "Logs" },
	];

	onMount(() => {
		const pararVarredura = onRescanProgress((p) => (progresso = p));
		const pararLeitura = onTreeProgress((retrato) => {
			raiz = retrato.root;
			versaoArvore += 1;
			// `done` apaga a barra, mas a raiz fica: ela É a árvore.
			lendo = retrato.done ? null : retrato;
		});
		return () => {
			pararVarredura.then((fn) => fn());
			pararLeitura.then((fn) => fn());
		};
	});

	/** Quanto da leitura já veio, de 0 a 100. */
	let porcentagem = $derived(
		lendo && lendo.total_estimate > 0
			? Math.min(100, Math.round((lendo.scanned / lendo.total_estimate) * 100))
			: 0,
	);

	/**
	 * Dispara a varredura do banco e NÃO espera por ela.
	 *
	 * Os retratos chegam por evento, a raiz aparece em menos de um segundo
	 * e os galhos já abrem com o que foi lido até ali — a varredura segue
	 * preenchendo o resto em segundo plano. Esperá-la terminar antes de
	 * desenhar seria deixar a tela muda por quase um minuto num banco de
	 * milhões de chaves.
	 */
	async function abrirBanco(hostId: string, banco: number) {
		erro = "";
		erroIndice = "";
		relatorio = null;
		perguntandoIndice = true;
		selecionada = null;
		raiz = [];
		lendo = { scanned: 0, total_estimate: 0, root: [], done: false };

		const minhaGeracao = ++geracaoIndice;
		scanStatus(hostId, banco)
			.then((r) => {
				// Uma troca de banco mais nova já partiu: esta resposta é de
				// uma pergunta que não interessa mais mostrar.
				if (minhaGeracao !== geracaoIndice) return;
				relatorio = r;
				erroIndice = "";
				perguntandoIndice = false;
			})
			.catch((err) => {
				if (minhaGeracao !== geracaoIndice) return;
				relatorio = null;
				erroIndice = errorMessage(err);
				perguntandoIndice = false;
			});

		try {
			await scanTree(hostId, banco);
		} catch (err) {
			erro = errorMessage(err);
		} finally {
			lendo = null;
		}
	}

	/**
	 * Uma conexão já sabida como falha não ganha uma segunda tentativa: sem
	 * isto, o `scan_tree` do Rust abre o próprio socket por conta própria e
	 * repete, em silêncio, a mesma falha que a linha do host já mostra —
	 * deixando a tela dizer "falhou" e "lendo as chaves" ao mesmo tempo, a
	 * própria confusão que esta tarefa existe para acabar.
	 */
	$effect(() => {
		if (falhaConexao) {
			erro = falhaConexao;
			erroIndice = "";
			perguntandoIndice = false;
			geracaoIndice += 1; // descarta qualquer pergunta de índice em voo
			selecionada = null;
			raiz = [];
			lendo = null;
			relatorio = null;
			return;
		}
		abrirBanco(host.id, db);
	});

	$effect(() => {
		const hostId = host.id;
		erroBancos = "";
		listDbs(hostId)
			.then((lista) => {
				bancos = lista;
				erroBancos = "";
			})
			.catch((err) => {
				bancos = [];
				erroBancos = errorMessage(err);
			});
	});

	async function varrer() {
		varrendo = true;
		progresso = null;
		erro = "";
		// Simétrico ao `erro` acima: sem isto, um "não foi possível
		// perguntar" de antes do clique sobrevivia debaixo de uma linha que
		// o próprio "Varrer de novo" acabou de corrigir. A geração também
		// sobe, para que uma resposta atrasada da pergunta de índice do
		// `abrirBanco` anterior não sobrescreva o relatório que este
		// `rescan` está prestes a trazer.
		erroIndice = "";
		perguntandoIndice = false;
		geracaoIndice += 1;
		try {
			relatorio = await rescan(host.id, db);
		} catch (err) {
			erro = errorMessage(err);
		} finally {
			varrendo = false;
			progresso = null;
		}
	}

	/**
	 * O Redis não guarda hash, lista ou sorted set vazios: uma chave desses
	 * tipos só passa a existir com ao menos um elemento dentro. Por isso a
	 * criação já entra com um elemento de partida, que a pessoa edita ou
	 * remove em seguida — a alternativa seria uma chave que some sozinha
	 * assim que a tela fosse recarregada.
	 */
	function opDeCriacao(kind: "string" | "hash" | "list" | "zset"): WriteOp {
		switch (kind) {
			case "string":
				return { op: "set_string", value: textBytes("") };
			case "hash":
				return { op: "set_hash_field", field: "campo", value: textBytes("") };
			case "list":
				return { op: "push_list_item", value: textBytes(""), front: false };
			case "zset":
				return { op: "set_zset_member", member: textBytes("membro"), score: 0 };
		}
	}

	async function criar(nova: { key: string; kind: "string" | "hash" | "list" | "zset" }) {
		const op = opDeCriacao(nova.kind);
		criando = false;
		await aplicarCriacao(nova.key, op, false);
	}

	async function aplicarCriacao(key: string, op: WriteOp, confirmado: boolean) {
		erro = "";
		try {
			const resultado = await writeValue(host.id, db, key, op, confirmado);
			if (resultado.outcome === "needs_confirmation") {
				criacaoPendente = { key, op, summary: resultado.summary };
				return;
			}
			criacaoPendente = null;
			selecionada = key;
		} catch (err) {
			criacaoPendente = null;
			erro = errorMessage(err);
		}
	}

	/** Uma escrita foi aplicada: relê a chave e atualiza a árvore. */
	function escreveu() {
		revisao += 1;
	}

	/** A chave sumiu entre a varredura e o clique: tira da seleção. */
	function chaveSumiu() {
		selecionada = null;
	}
</script>

<div class="trabalho" style="--largura-chaves: {larguraChaves}px">
	<div class="coluna">
		<div class="controles">
			<label class="banco">
				<span>Banco</span>
				<select bind:value={db}>
					{#if bancos.length === 0}
						<option value={db}>{db}</option>
					{:else}
						{#each bancos as [indice, chaves] (indice)}
							<option value={indice}>{indice} — {numero(chaves)} chaves</option>
						{/each}
					{/if}
				</select>
			</label>
			{#if erroBancos}
				<p class="erro" role="alert" data-testid="erro-bancos">{erroBancos}</p>
			{/if}

			<div class="busca-linha">
				<input
					class="filtro"
					bind:value={filtro}
					placeholder="Filtrar o que está aberto"
					aria-label="Filtrar chaves por nome"
				/>
				<button onclick={() => (criando = true)}>Nova chave</button>
			</div>

			<div class="varredura">
				<span class="idade">
					{#if varrendo && progresso}
						Varrendo {numero(progresso.scanned)} de
						{numero(progresso.total_estimate)}
					{:else if varrendo}
						Varrendo…
					{:else if relatorio}
						Índice de busca: {haQuantoTempo(Math.max(0, Math.round((Date.now() - relatorio.finished_at) / 1000)))}
					{:else if erroIndice}
						Índice de busca: não foi possível perguntar
					{:else if perguntandoIndice}
						Índice de busca: perguntando ao servidor…
					{:else}
						Índice de busca: nunca varrido
					{/if}
				</span>
				<button onclick={varrer} disabled={varrendo}>Varrer de novo</button>
			</div>
			{#if erroIndice}
				<p class="erro" role="alert" data-testid="erro-indice">{erroIndice}</p>
			{/if}
		</div>

		{#if lendo}
			<div class="lendo" data-testid="lendo">
				<p class="lendo-linha">
					Lendo as chaves em segundo plano
					{#if lendo.total_estimate > 0}
						<span class="mono">
							{porcentagem}% ({numero(lendo.scanned)} de {numero(lendo.total_estimate)})
						</span>
					{/if}
				</p>
				<div
					class="barra"
					role="progressbar"
					aria-valuenow={porcentagem}
					aria-valuemin="0"
					aria-valuemax="100"
					aria-label="Progresso da leitura das chaves"
				>
					<div class="barra-cheia" style="width: {porcentagem}%"></div>
				</div>
			</div>
		{/if}

		<div class="arvore">
			<KeyTree
				hostId={host.id}
				{db}
				root={raiz}
				version={versaoArvore}
				selectedKey={selecionada}
				onSelect={(key) => (selecionada = key)}
				filter={filtro}
			/>
		</div>
	</div>

	<Divisor
		chave="largura:chaves"
		inicial={LARGURA_CHAVES_PADRAO}
		onLargura={(px) => (larguraChaves = px)}
	/>

	<div class="painel">
		<nav class="abas">
			{#each ABAS as item (item.id)}
				<button
					class="aba"
					class:ativa={aba === item.id}
					aria-current={aba === item.id ? "page" : undefined}
					onclick={() => (aba = item.id)}
				>
					{item.label}
				</button>
			{/each}
		</nav>

		<div class="conteudo">
		{#if aba === "busca"}
			<SearchPanel
				hostId={host.id}
				{db}
				onOpenKey={(key) => {
					selecionada = key;
					aba = "valor";
				}}
			/>
		{:else if aba === "historico"}
			<HistoryPanel hostId={host.id} onRolledBack={escreveu} />
		{:else if aba === "logs"}
			<LogPanel />
		{:else if erro}
			<p class="erro" role="alert">{erro}</p>
		{:else if selecionada}
			{#key `${selecionada}#${revisao}`}
				<ValuePanel
					hostId={host.id}
					{db}
					keyName={selecionada}
					onKeyGone={chaveSumiu}
				>
					{#snippet editor(view)}
						<KeyEditor hostId={host.id} {db} {view} onWritten={escreveu} />
					{/snippet}
				</ValuePanel>
			{/key}
		{:else}
			<p class="aviso">Escolha uma chave na árvore.</p>
		{/if}
		</div>
	</div>
</div>

{#if criando}
	<NewKeyDialog onCreate={criar} onCancel={() => (criando = false)} />
{/if}

{#if criacaoPendente}
	<ConfirmDialog
		summary={criacaoPendente.summary}
		onConfirm={() =>
			criacaoPendente &&
			aplicarCriacao(criacaoPendente.key, criacaoPendente.op, true)}
		onCancel={() => (criacaoPendente = null)}
	/>
{/if}

<style>
	.trabalho {
		display: grid;
		/* A variável vem do `Divisor`: ele é quem mede o arrasto e lembra a
		   largura entre sessões. O valor fixo aqui só cobre o instante
		   antes da alça montar. */
		grid-template-columns: var(--largura-chaves, 16rem) 1fr;
		height: 100%;
		min-height: 0;
		/* Ancora o `Divisor`, que se posiciona sozinho em cima da borda
		   entre as colunas sem ocupar uma faixa própria na grade. */
		position: relative;
	}

	.coluna {
		display: flex;
		flex-direction: column;
		min-height: 0;
		border-right: 1px solid var(--rule);
	}

	.controles {
		display: flex;
		flex-direction: column;
		gap: 0.45rem;
		padding: 0.6rem 0.7rem;
		border-bottom: 1px solid var(--rule);
	}

	.banco {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}

	.banco > span {
		color: var(--text-dim);
		font-size: 0.9rem;
	}

	.varredura {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
	}

	.idade {
		color: var(--text-faint);
		font-size: 0.88rem;
	}

	.varredura button {
		padding: 0.12rem 0.45rem;
		font-size: 0.88rem;
		color: var(--text-dim);
	}

	.busca-linha {
		display: flex;
		gap: 0.4rem;
	}

	.busca-linha button {
		white-space: nowrap;
	}

	.arvore {
		flex: 1;
		min-height: 0;
	}

	.painel {
		display: flex;
		flex-direction: column;
		min-width: 0;
		min-height: 0;
	}

	.abas {
		display: flex;
		gap: 0.2rem;
		padding: 0 1.2rem;
		border-bottom: 1px solid var(--rule);
	}

	.aba {
		padding: 0.45rem 0.7rem;
		border: none;
		border-radius: 0;
		border-bottom: 2px solid transparent;
		color: var(--text-faint);
	}

	.aba:hover {
		color: var(--text-dim);
	}

	.ativa {
		color: var(--text);
		border-bottom-color: var(--action);
	}

	.conteudo {
		flex: 1;
		min-height: 0;
		padding: 1rem 1.2rem;
		overflow-y: auto;
	}

	.aviso {
		margin: 0;
		padding: 0.8rem 0.2rem;
		color: var(--text-faint);
	}

	.lendo {
		padding: 0.8rem;
	}

	.lendo-linha {
		margin: 0 0 0.45rem;
		color: var(--text-dim);
		font-size: 0.92rem;
	}

	/* Barra fina, sem animação própria: ela já se move sozinha conforme os
	   lotes chegam, e um brilho correndo por cima só competiria com isso. */
	.barra {
		height: 3px;
		background: var(--panel-dark);
		border-radius: 999px;
		overflow: hidden;
	}

	.barra-cheia {
		height: 100%;
		background: var(--action);
		transition: width 120ms linear;
	}

	.erro {
		margin: 0;
		padding: 0.4rem 0.6rem;
		border: 1px solid #5e3a38;
		border-radius: var(--radius);
		background: #2e2326;
		color: #e49a94;
	}

	@media (max-width: 52rem) {
		.trabalho {
			grid-template-columns: 1fr;
			grid-template-rows: minmax(12rem, 40%) 1fr;
		}

		.coluna {
			border-right: none;
			border-bottom: 1px solid var(--rule);
		}

		/* Sem colunas lado a lado, não há borda para a alça marcar. */
		.trabalho :global(.divisor) {
			display: none;
		}
	}
</style>
