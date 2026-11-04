<script lang="ts">
	/**
	 * A área de trabalho de um host aberto: escolha do banco, árvore de
	 * chaves à esquerda, painel do valor à direita.
	 */
	import ConfirmDialog from "$lib/ConfirmDialog.svelte";
	import KeyEditor from "$lib/KeyEditor.svelte";
	import KeyTree from "$lib/KeyTree.svelte";
	import NewKeyDialog from "$lib/NewKeyDialog.svelte";
	import SearchPanel from "$lib/SearchPanel.svelte";
	import ValuePanel from "$lib/ValuePanel.svelte";
	import { haQuantoTempo, numero } from "$lib/formato";
	import {
		errorMessage,
		listDbs,
		loadTree,
		onRescanProgress,
		rescan,
		scanStatus,
		textBytes,
		writeValue,
		type Host,
		type ScanProgress,
		type ScanReport,
		type TreeNode,
		type WriteOp,
	} from "$lib/api";
	import { onMount, untrack } from "svelte";

	interface Props {
		host: Host;
	}

	let { host }: Props = $props();

	/**
	 * O banco parte do que o host traz cadastrado e passa a ser escolha da
	 * pessoa — recarregar a lista de hosts no fundo não pode puxá-la de
	 * volta para o banco do cadastro. Trocar de host é remontagem, feita com
	 * `{#key}` por quem usa este componente.
	 */
	let db = $state(untrack(() => host.db));
	let bancos = $state<[number, number][]>([]);
	let nodes = $state<TreeNode[]>([]);
	let filtro = $state("");
	let selecionada = $state<string | null>(null);
	let relatorio = $state<ScanReport | null>(null);
	let progresso = $state<ScanProgress | null>(null);
	let carregando = $state(false);
	let varrendo = $state(false);
	let erro = $state("");
	let criando = $state(false);
	/** Criação recusada em produção, esperando confirmação. */
	let criacaoPendente = $state<{ key: string; op: WriteOp; summary: string } | null>(null);
	/** Força o painel a reler a chave depois de uma escrita. */
	let revisao = $state(0);

	type Aba = "valor" | "busca";
	let aba = $state<Aba>("valor");
	const ABAS: { id: Aba; label: string }[] = [
		{ id: "valor", label: "Valor" },
		{ id: "busca", label: "Busca" },
	];

	onMount(() => {
		const parar = onRescanProgress((p) => (progresso = p));
		return () => {
			parar.then((fn) => fn());
		};
	});

	async function carregarArvore(hostId: string, banco: number) {
		carregando = true;
		erro = "";
		selecionada = null;
		try {
			nodes = await loadTree(hostId, banco);
			relatorio = await scanStatus(hostId, banco);
		} catch (err) {
			nodes = [];
			erro = errorMessage(err);
		} finally {
			carregando = false;
		}
	}

	$effect(() => {
		carregarArvore(host.id, db);
	});

	$effect(() => {
		const hostId = host.id;
		listDbs(hostId)
			.then((lista) => (bancos = lista))
			.catch(() => (bancos = []));
	});

	async function varrer() {
		varrendo = true;
		progresso = null;
		erro = "";
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
			await carregarArvore(host.id, db);
			selecionada = key;
		} catch (err) {
			criacaoPendente = null;
			erro = errorMessage(err);
		}
	}

	/** Uma escrita foi aplicada: relê a chave e atualiza a árvore. */
	async function escreveu() {
		const chave = selecionada;
		await carregarArvore(host.id, db);
		selecionada = chave;
		revisao += 1;
	}

	/** A chave sumiu entre a varredura e o clique: tira da árvore. */
	function chaveSumiu() {
		selecionada = null;
		carregarArvore(host.id, db);
	}
</script>

<div class="trabalho">
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

			<div class="busca-linha">
				<input
					class="filtro"
					bind:value={filtro}
					placeholder="Filtrar por nome"
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
					{:else}
						Índice de busca: nunca varrido
					{/if}
				</span>
				<button onclick={varrer} disabled={varrendo}>Varrer de novo</button>
			</div>
		</div>

		<div class="arvore">
			{#if carregando}
				<p class="aviso">Lendo as chaves…</p>
			{:else}
				<KeyTree
					{nodes}
					selectedKey={selecionada}
					onSelect={(key) => (selecionada = key)}
					filter={filtro}
				/>
			{/if}
		</div>
	</div>

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
		grid-template-columns: minmax(16rem, 24rem) 1fr;
		height: 100%;
		min-height: 0;
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
	}
</style>
