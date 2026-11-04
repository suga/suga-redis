<script lang="ts">
	/**
	 * A área de trabalho de um host aberto: escolha do banco, árvore de
	 * chaves à esquerda, painel do valor à direita.
	 */
	import KeyTree from "$lib/KeyTree.svelte";
	import ValuePanel from "$lib/ValuePanel.svelte";
	import {
		errorMessage,
		listDbs,
		loadTree,
		onRescanProgress,
		rescan,
		scanStatus,
		type Host,
		type ScanProgress,
		type ScanReport,
		type TreeNode,
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

	/** Quanto tempo faz desde a última varredura, em palavras. */
	function idade(finishedAt: number): string {
		const segundos = Math.max(0, Math.round((Date.now() - finishedAt) / 1000));
		if (segundos < 60) return "agora há pouco";
		const minutos = Math.round(segundos / 60);
		if (minutos < 60) return `há ${minutos} min`;
		const horas = Math.round(minutos / 60);
		if (horas < 24) return `há ${horas} h`;
		return `há ${Math.round(horas / 24)} dias`;
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
						{#each bancos as [numero, chaves] (numero)}
							<option value={numero}>{numero} — {chaves.toLocaleString("pt-BR")} chaves</option>
						{/each}
					{/if}
				</select>
			</label>

			<input
				class="filtro"
				bind:value={filtro}
				placeholder="Filtrar por nome"
				aria-label="Filtrar chaves por nome"
			/>

			<div class="varredura">
				<span class="idade">
					{#if varrendo && progresso}
						Varrendo {progresso.scanned.toLocaleString("pt-BR")} de
						{progresso.total_estimate.toLocaleString("pt-BR")}
					{:else if varrendo}
						Varrendo…
					{:else if relatorio}
						Índice de busca: {idade(relatorio.finished_at)}
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
		{#if erro}
			<p class="erro" role="alert">{erro}</p>
		{:else if selecionada}
			{#key selecionada}
				<ValuePanel
					hostId={host.id}
					{db}
					keyName={selecionada}
					onKeyGone={chaveSumiu}
				/>
			{/key}
		{:else}
			<p class="aviso">Escolha uma chave na árvore.</p>
		{/if}
	</div>
</div>

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

	.arvore {
		flex: 1;
		min-height: 0;
	}

	.painel {
		padding: 1rem 1.2rem;
		overflow-y: auto;
		min-width: 0;
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
