<script lang="ts">
	import { t } from "$lib/i18n.svelte";
	/**
	 * A alça entre duas colunas de um layout em grade: arrasta para
	 * redimensionar, e lembra a largura escolhida entre uma sessão e outra
	 * (`localStorage`, não SQLite — é preferência de janela, não dado do
	 * produto, e perder a lembrança custa só um arrasto).
	 *
	 * Esta alça não desenha a grade: ela só mede e avisa. Quem a usa decide
	 * como aplicar `onLargura` — tipicamente numa variável CSS lida pelo
	 * `grid-template-columns`, para que a regra de empilhamento em telas
	 * estreitas continue podendo vencer.
	 */
	import { onDestroy, onMount } from "svelte";

	interface Props {
		/** Chave no localStorage, ex.: "largura:hosts" | "largura:chaves". */
		chave: string;
		/** Largura em px usada quando não há nada guardado, ou o guardado é inválido. */
		inicial: number;
		/** Largura mínima em px. */
		min?: number;
		onLargura: (px: number) => void;
	}

	let { chave, inicial, min = 192, onLargura }: Props = $props();

	/** O passo de um toque de seta — pensado para quem não usa mouse. */
	const PASSO = 16;

	/** A própria alça, para medir o espaço que ela divide. */
	let elemento: HTMLDivElement | undefined = $state();

	/**
	 * Teto da largura: metade do contêiner que a alça divide, recalculado a
	 * cada movimento — nunca uma coluna maior que a metade do espaço que ela
	 * tem para repartir.
	 *
	 * O contêiner, e não a janela: a alça das chaves vive DENTRO do painel
	 * da direita, e medir a janela dava a ela o mesmo teto da alça dos
	 * hosts. Com a coluna de hosts no máximo, isso deixava o painel do
	 * valor espremido a quase nada.
	 */
	function teto(): number {
		const painel = elemento?.parentElement?.clientWidth ?? 0;
		// Antes de montar (a largura inicial é lida aí) e num ambiente sem
		// layout, o pai mede 0: a janela é a melhor referência disponível, e
		// é a que valia antes desta medida existir.
		return (painel > 0 ? painel : window.innerWidth) / 2;
	}

	/**
	 * O teto corrente, anunciado a quem navega por teclado. É estado, e não
	 * uma chamada direta no markup: `aria-valuemax={teto()}` era avaliado
	 * uma vez e nunca mais, prometendo a quem lê um limite que já não era o
	 * de agora.
	 */
	let maximo = $state(0);

	function dentroDosLimites(valor: number, limite: number): boolean {
		return Number.isFinite(valor) && valor >= min && valor <= limite;
	}

	/** Lê e valida o que está guardado; qualquer coisa fora do esperado cai
	 * no `inicial` — uma coluna arrastada a nada não teria alça para voltar. */
	function larguraInicial(): number {
		const guardada = Number(localStorage.getItem(chave));
		return dentroDosLimites(guardada, teto()) ? guardada : inicial;
	}

	let largura = $state(larguraInicial());

	onMount(() => {
		// Agora o elemento existe e o contêiner pode ser medido: a largura
		// inicial foi validada antes disso, contra a janela. Reaplicar
		// devolve a largura já sabida (guardada ou inicial) a quem usa a
		// alça, já limitada pelo espaço real.
		maximo = teto();
		aplicar(largura);

		// A janela muda de tamanho e o teto muda com ela — inclusive o que
		// está anunciado na alça.
		const aoRedimensionar = () => {
			maximo = teto();
			aplicar(largura);
		};
		window.addEventListener("resize", aoRedimensionar);
		return () => window.removeEventListener("resize", aoRedimensionar);
	});

	function aplicar(px: number) {
		maximo = teto();
		const limitada = Math.min(Math.max(px, min), maximo);
		largura = limitada;
		onLargura(limitada);
		return limitada;
	}

	/** Solta o arrasto em curso — exposta fora de `iniciarArrasto` para o
	 * `onDestroy` poder chamá-la se a alça sumir no meio do caminho (o
	 * `{#key}` da Task 11 remonta o Workspace ao trocar de host). */
	let soltarArrasto: (() => void) | null = null;

	function iniciarArrasto(evento: MouseEvent) {
		evento.preventDefault();
		const xInicial = evento.clientX;
		const larguraInicialDoArrasto = largura;

		// Visível além da alça de 7px: sem isto, um arrasto rápido ultrapassa
		// a alça e perde o cursor de redimensionar no meio do gesto.
		document.body.style.cursor = "col-resize";

		function mover(e: MouseEvent) {
			if (e.buttons === 0) {
				// O botão já foi solto, e nenhum `mouseup` chegou até a
				// página — o normal ao soltar fora de uma janela do Tauri
				// que não está maximizada. Sem este desvio, os listeners
				// ficam presos no `window` e a coluna continua "arrastando"
				// sozinha na próxima vez que o ponteiro cruzar a janela.
				soltar();
				return;
			}
			aplicar(larguraInicialDoArrasto + (e.clientX - xInicial));
		}

		function soltar() {
			localStorage.setItem(chave, String(largura));
			document.body.style.cursor = "";
			window.removeEventListener("mousemove", mover);
			window.removeEventListener("mouseup", soltar);
			soltarArrasto = null;
		}

		window.addEventListener("mousemove", mover);
		window.addEventListener("mouseup", soltar);
		soltarArrasto = soltar;
	}

	onDestroy(() => soltarArrasto?.());

	function teclado(evento: KeyboardEvent) {
		if (evento.key !== "ArrowRight" && evento.key !== "ArrowLeft") {
			return;
		}
		evento.preventDefault();
		const sinal = evento.key === "ArrowRight" ? 1 : -1;
		const nova = aplicar(largura + sinal * PASSO);
		localStorage.setItem(chave, String(nova));
	}
</script>

<!-- Vermelho e CAIXA ALTA são vocabulário reservado ao ambiente de produção
     em toda a tela — esta alça não usa nenhum dos dois. -->
<!-- O papel "separator" é estrutural por padrão na tabela que o Svelte
     usa — por isso os dois avisos a seguir. Mas o "Window Splitter" do
     WAI-ARIA Authoring Practices é exatamente este caso: separator com
     tabindex e aria-valuenow, focável e arrastável de propósito. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
	bind:this={elemento}
	class="divisor"
	style="left: {largura}px"
	role="separator"
	aria-orientation="vertical"
	aria-label={t("divisor.redimensionar")}
	aria-valuenow={largura}
	aria-valuemin={min}
	aria-valuemax={maximo}
	tabindex="0"
	onmousedown={iniciarArrasto}
	onkeydown={teclado}
></div>

<style>
	.divisor {
		position: absolute;
		top: 0;
		bottom: 0;
		width: 7px;
		margin-left: -3px;
		cursor: col-resize;
		z-index: 1;
		background: transparent;
	}

	.divisor:hover,
	.divisor:focus-visible {
		background: var(--rule);
		outline: none;
	}
</style>
