<script lang="ts" module>
	export interface Item {
		id: string;
		rotulo: string;
		/** Marca uma ação destrutiva. Não usa `--prod`: aquele vermelho significa
		 * só o ambiente, e emprestá-lo aqui deixaria o aviso de produção ambíguo. */
		perigoso?: boolean;
	}
</script>

<script lang="ts">
	/**
	 * Um menu de contexto genérico: recebe uma posição e uma lista de itens,
	 * e avisa qual foi escolhido. Não sabe nada de chaves, prefixos ou
	 * exclusão — quem monta a lista e decide o que cada `id` significa é
	 * quem o chama.
	 *
	 * Abre onde o mouse clicou, mas vira para cima quando não cabe abaixo
	 * do cursor, e para a esquerda quando não cabe à direita — um menu
	 * desenhado fora da tela é um menu com item que ninguém alcança.
	 *
	 * Também serve quem opera por teclado: as setas andam entre os itens,
	 * Enter escolhe, e Escape fecha devolvendo o foco a quem abriu o menu —
	 * do jeito que `ConfirmDialog` já faz, para quem abriu sem querer não
	 * se perder na tela.
	 */
	import { onMount, tick } from "svelte";

	interface Props {
		x: number;
		y: number;
		itens: Item[];
		onEscolher: (id: string) => void;
		onFechar: () => void;
	}

	let { x, y, itens, onEscolher, onFechar }: Props = $props();

	/** Quem tinha o foco antes do menu abrir — Escape devolve o foco a ele. */
	const abridor = document.activeElement as HTMLElement | null;

	let raiz: HTMLElement | undefined = $state();
	let botoes: HTMLButtonElement[] = $state([]);
	/** Nenhum item em foco até a primeira seta — por isso -1, não 0. */
	let focado = $state(-1);

	/** Viragem calculada uma vez, na montagem: a janela não muda sob o menu. */
	let estilo = $state("");

	onMount(() => {
		const altura = raiz?.offsetHeight ?? 0;
		const largura = raiz?.offsetWidth ?? 0;

		const viraParaCima = y + altura > window.innerHeight;
		const viraParaEsquerda = x + largura > window.innerWidth;

		const vertical = viraParaCima
			? `bottom: ${window.innerHeight - y}px;`
			: `top: ${y}px;`;
		const horizontal = viraParaEsquerda
			? `right: ${window.innerWidth - x}px;`
			: `left: ${x}px;`;

		estilo = `${vertical} ${horizontal}`;
	});

	function fechar() {
		onFechar();
		abridor?.focus();
	}

	function escolher(id: string) {
		onEscolher(id);
	}

	async function mover(delta: number) {
		const total = itens.length;
		if (total === 0) return;
		focado = focado === -1 ? 0 : (focado + delta + total) % total;
		await tick();
		botoes[focado]?.focus();
	}

	function aoTeclar(e: KeyboardEvent) {
		switch (e.key) {
			case "Escape":
				e.preventDefault();
				fechar();
				break;
			case "ArrowDown":
				e.preventDefault();
				mover(1);
				break;
			case "ArrowUp":
				e.preventDefault();
				mover(-1);
				break;
			case "Enter":
			case " ":
				if (focado !== -1) {
					e.preventDefault();
					escolher(itens[focado].id);
				}
				break;
		}
	}

	function aoClicarFora(e: MouseEvent) {
		if (raiz && !raiz.contains(e.target as Node)) {
			fechar();
		}
	}
</script>

<svelte:window onkeydown={aoTeclar} onmousedown={aoClicarFora} />

<div bind:this={raiz} class="menu" role="menu" style={estilo}>
	{#each itens as item, i (item.id)}
		<button
			bind:this={botoes[i]}
			type="button"
			role="menuitem"
			class="item"
			class:perigoso={item.perigoso}
			tabindex={focado === i ? 0 : -1}
			onclick={() => escolher(item.id)}
			onfocus={() => (focado = i)}
		>
			{item.rotulo}
		</button>
	{/each}
</div>

<style>
	.menu {
		position: fixed;
		z-index: 20;
		min-width: 10rem;
		padding: 0.25rem;
		display: flex;
		flex-direction: column;
		background: var(--panel-dark);
		border: 1px solid var(--rule-strong);
		border-radius: var(--radius);
	}

	.item {
		all: unset;
		box-sizing: border-box;
		padding: 0.4rem 0.6rem;
		color: var(--text);
		font-family: var(--ui);
		font-size: 0.9rem;
		border-radius: var(--radius);
		cursor: pointer;
	}

	.item:hover,
	.item:focus-visible {
		background: var(--rule-strong);
	}

	/* Perigoso se distingue por peso e por um traço acima, não por cor: o
	   vermelho de `--prod` fica reservado só para o ambiente, e usá-lo aqui
	   deixaria o aviso de produção ambíguo. */
	.item.perigoso {
		margin-top: 0.25rem;
		padding-top: calc(0.4rem + 1px);
		border-top: 1px solid var(--rule-strong);
		font-weight: 600;
	}
</style>
