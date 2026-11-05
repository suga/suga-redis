<script lang="ts">
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

	/** Teto da largura: metade da janela, recalculado a cada movimento —
	 * nunca uma coluna maior que a própria tela. */
	function teto(): number {
		return window.innerWidth / 2;
	}

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
		// Devolve a largura já sabida (guardada ou inicial) assim que a
		// alça nasce, para quem a usa pintar a grade com ela desde já.
		onLargura(largura);
	});

	function aplicar(px: number) {
		const limitada = Math.min(Math.max(px, min), teto());
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
	class="divisor"
	style="left: {largura}px"
	role="separator"
	aria-orientation="vertical"
	aria-label="Redimensionar coluna"
	aria-valuenow={largura}
	aria-valuemin={min}
	aria-valuemax={teto()}
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
