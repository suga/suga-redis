<script lang="ts">
	/**
	 * A árvore de chaves, agrupada por prefixo.
	 *
	 * Desenha só as linhas que cabem na janela de rolagem. Um banco com
	 * dezenas de milhares de chaves vira dezenas de milhares de nós na
	 * árvore, e pôr todos no DOM trava a janela inteira — inclusive a barra
	 * do ambiente, que é justamente o que não pode sumir.
	 */
	import { SvelteSet } from "svelte/reactivity";
	import type { TreeNode } from "$lib/api";

	interface Props {
		nodes: TreeNode[];
		selectedKey: string | null;
		onSelect: (key: string) => void;
		/** Filtro por nome. Vazio mostra a árvore inteira. */
		filter: string;
	}

	let { nodes, selectedKey, onSelect, filter }: Props = $props();

	/** Altura fixa de cada linha, em pixels. É o que torna a janela calculável. */
	const ROW_HEIGHT = 26;
	/** Linhas desenhadas além da janela, para a rolagem não mostrar buraco. */
	const OVERSCAN = 8;

	let abertos = $state(new SvelteSet<string>());
	let scrollTop = $state(0);
	let viewport = $state(0);

	/**
	 * Mantém um galho quando ele mesmo casa com o filtro ou quando sobra
	 * algum descendente: o caminho até um acerto precisa continuar
	 * navegável, senão o acerto fica inalcançável. Galho que casa pelo
	 * próprio nome mantém os filhos todos — quem procurou por ele quer ver
	 * o que tem dentro.
	 */
	function filtrar(lista: TreeNode[], alvo: string): TreeNode[] {
		const resultado: TreeNode[] = [];
		for (const node of lista) {
			const casa =
				node.label.toLowerCase().includes(alvo) ||
				(node.leaf_key?.toLowerCase().includes(alvo) ?? false);
			const filhos = filtrar(node.children, alvo);
			if (casa) {
				resultado.push(node);
			} else if (filhos.length > 0) {
				resultado.push({ ...node, children: filhos });
			}
		}
		return resultado;
	}

	let visivel = $derived(
		filter.trim() ? filtrar(nodes, filter.trim().toLowerCase()) : nodes,
	);

	interface Row {
		node: TreeNode;
		depth: number;
		open: boolean;
	}

	/**
	 * Achata a árvore nas linhas que estão à vista. Com filtro ativo tudo
	 * que sobrou já vem aberto: a pessoa filtrou para VER os acertos, não
	 * para abrir galho por galho até chegar neles.
	 */
	function achatar(lista: TreeNode[], depth: number, out: Row[]): Row[] {
		for (const node of lista) {
			const folha = node.leaf_key !== null;
			const open = folha ? false : filtrandoAgora || abertos.has(node.full_prefix);
			out.push({ node, depth, open });
			if (open) achatar(node.children, depth + 1, out);
		}
		return out;
	}

	let filtrandoAgora = $derived(filter.trim().length > 0);
	let linhas = $derived(achatar(visivel, 0, []));

	/**
	 * A fatia a desenhar. Enquanto a janela não foi medida (primeira pintura,
	 * e também em teste fora de um navegador de verdade) `viewport` é zero —
	 * aí desenha tudo, que é o comportamento correto para as árvores
	 * pequenas e nunca esconde linha de ninguém.
	 */
	let inicio = $derived(
		viewport === 0 ? 0 : Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN),
	);
	let fim = $derived(
		viewport === 0
			? linhas.length
			: Math.min(
					linhas.length,
					Math.ceil((scrollTop + viewport) / ROW_HEIGHT) + OVERSCAN,
				),
	);
	let fatia = $derived(linhas.slice(inicio, fim));

	function alternar(node: TreeNode) {
		if (abertos.has(node.full_prefix)) abertos.delete(node.full_prefix);
		else abertos.add(node.full_prefix);
	}

	function medir(el: HTMLElement) {
		viewport = el.clientHeight;
		const observer = new ResizeObserver(() => (viewport = el.clientHeight));
		observer.observe(el);
		return { destroy: () => observer.disconnect() };
	}

</script>

<div
	class="rolagem"
	use:medir
	onscroll={(e) => (scrollTop = e.currentTarget.scrollTop)}
>
	{#if linhas.length === 0}
		<p class="vazio">
			{filtrandoAgora ? "Nenhuma chave com esse nome." : "Nenhuma chave neste banco."}
		</p>
	{:else}
		<div class="pista" style="height: {linhas.length * ROW_HEIGHT}px">
			<div class="janela" style="transform: translateY({inicio * ROW_HEIGHT}px)">
				{#each fatia as row (row.node.full_prefix)}
					{@const folha = row.node.leaf_key}
					<button
						class="linha"
						class:selecionada={folha !== null && folha === selectedKey}
						style="padding-left: {0.5 + row.depth * 0.85}rem"
						onclick={() => (folha !== null ? onSelect(folha) : alternar(row.node))}
					>
						<span class="caret" aria-hidden="true">
							{folha !== null ? "" : row.open ? "▾" : "▸"}
						</span>
						<span class="rotulo">{row.node.label}</span>
						{#if folha === null}
							<span class="contagem mono">{row.node.count}</span>
						{/if}
					</button>
				{/each}
			</div>
		</div>
	{/if}
</div>

<style>
	.rolagem {
		height: 100%;
		overflow-y: auto;
	}

	.vazio {
		margin: 0;
		padding: 1rem 0.8rem;
		color: var(--text-faint);
	}

	/* A pista tem a altura da árvore INTEIRA, para a barra de rolagem
	   corresponder ao conteúdo real; a janela desliza dentro dela. */
	.pista {
		position: relative;
	}

	.janela {
		position: absolute;
		top: 0;
		left: 0;
		right: 0;
	}

	.linha {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		width: 100%;
		height: 26px;
		padding-right: 0.6rem;
		border: none;
		border-radius: 0;
		text-align: left;
		white-space: nowrap;
	}

	.linha:hover {
		background: var(--panel-dark);
	}

	.selecionada {
		background: var(--panel-dark);
		box-shadow: inset 2px 0 0 var(--action);
	}

	.caret {
		width: 0.8rem;
		color: var(--text-faint);
		flex: none;
	}

	.rotulo {
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.contagem {
		margin-left: auto;
		color: var(--text-faint);
		font-size: 0.85em;
	}
</style>
