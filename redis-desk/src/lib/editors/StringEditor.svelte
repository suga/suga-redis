<script lang="ts">
	/**
	 * Editor de valor do tipo `string`, colorido ENQUANTO se edita.
	 *
	 * O truque é uma sobreposição: um `<textarea>` de verdade por cima, com
	 * o texto transparente e só o cursor visível, e por baixo um `<pre>` com
	 * os mesmos caracteres pintados por tipo. Quem digita mexe no textarea;
	 * quem enxerga lê a camada de baixo.
	 *
	 * Isso exige que as duas camadas desenhem o texto EXATAMENTE igual —
	 * mesma fonte, tamanho, entrelinha, preenchimento, largura de borda e
	 * regra de quebra. Qualquer diferença aparece como cursor fora do lugar,
	 * e é por isso que essas propriedades moram numa regra só, compartilhada
	 * pelas duas.
	 *
	 * A alternativa seria um editor de código de verdade (CodeMirror e
	 * afins): algumas centenas de kilobytes para colorir quatro tipos de
	 * token num campo.
	 */
	import { untrack } from "svelte";
	import { formatarJson, pareceJson, realcarJson, validarJson } from "$lib/json";
	import { numero } from "$lib/formato";
	import { textBytes, type ValueView, type WriteOp } from "$lib/api";

	interface Props {
		view: ValueView;
		onSubmit: (op: WriteOp) => void;
		busy?: boolean;
	}

	let { view, onSubmit, busy = false }: Props = $props();

	/**
	 * Acima disto o texto deixa de ser colorido enquanto se edita.
	 *
	 * Cada tecla redesenha a camada de cor, e num valor grande demais isso
	 * passa a ser sentido na digitação. Entre colorir e manter o campo
	 * respondendo, o campo ganha: ler sem cor incomoda, digitar com atraso
	 * inviabiliza.
	 */
	const TETO_REALCE_BYTES = 256 * 1024;

	let dado = $derived(view.value.kind === "string" ? view.value.data : null);

	/**
	 * Valor que não é UTF-8 não entra num campo de texto: o que voltasse de
	 * lá já não seriam os mesmos bytes.
	 */
	let binario = $derived(dado !== null && dado.text === null);

	/** O texto que o servidor entregou nesta leitura. */
	let servidor = $derived(dado?.text ?? "");

	let texto = $state(
		untrack(() => (view.value.kind === "string" ? (view.value.data.text ?? "") : "")),
	);

	/** Em cima de qual texto do servidor o campo foi montado. */
	let base = $state(
		untrack(() => (view.value.kind === "string" ? (view.value.data.text ?? "") : "")),
	);

	/**
	 * O que está no campo desceu de uma leitura INCOMPLETA.
	 *
	 * Não é o mesmo que `view.truncated`: "Carregar mais" traz o resto do
	 * valor e apaga o `truncated`, mas o campo continua com o pedaço antigo
	 * — `$state` vindo de prop captura só o valor inicial. Enquanto essa
	 * diferença existir, salvar gravaria o pedaço por cima do inteiro.
	 */
	let baseCortada = $state(untrack(() => view.truncated));

	/**
	 * Adota o texto novo quando o servidor entrega mais do mesmo valor.
	 *
	 * Só adota se o campo estiver INTOCADO: o que a pessoa digitou é a
	 * única cópia daquele trabalho, e substituí-lo por causa de uma
	 * paginação perderia a edição dela. Se ela já mexeu, o texto fica — e
	 * `baseCortada` fica junto, porque a edição dela descende do pedaço.
	 */
	$effect(() => {
		const entregue = servidor;
		const cortadoAgora = view.truncated;
		untrack(() => {
			if (entregue === base) return;
			if (texto === base) {
				texto = entregue;
				baseCortada = cortadoAgora;
			}
			base = entregue;
		});
	});

	/**
	 * O que está na tela é só um pedaço do que está no servidor.
	 *
	 * Enquanto for assim, salvar é PROIBIDO: gravaria o pedaço por cima do
	 * inteiro e apagaria o resto sem aviso. Foi exatamente o que quase
	 * aconteceu com um valor de 112 kB do qual só 64 kB tinham chegado.
	 */
	let cortado = $derived(view.truncated || baseCortada);

	/**
	 * Num valor cortado a validação fica calada: o começo de um JSON grande
	 * nunca fecha, e acusar erro de sintaxe seria culpar o conteúdo por um
	 * corte que foi nosso.
	 */
	let erroJson = $derived(cortado ? null : validarJson(texto));
	let indentado = $derived(cortado ? null : formatarJson(texto));

	/**
	 * Formatar é VISUALIZAÇÃO, não edição.
	 *
	 * Quem aperta "Formatar JSON" quer ler um valor espremido, não
	 * reescrevê-lo. Se o indentado fosse direto para o servidor, salvar
	 * trocaria o valor inteiro por causa de espaço em branco que ninguém
	 * pediu — e num valor de 100 kB isso é uma escrita inteira de graça.
	 *
	 * Guarda-se o par: o texto original e exatamente o indentado que saiu
	 * dele. Enquanto o campo ainda for aquele indentado, o que se grava é o
	 * original. Qualquer tecla a mais rompe a igualdade e o que está na
	 * tela volta a ser o que vale.
	 */
	let formatado = $state<{ de: string; para: string } | null>(null);

	let aSalvar = $derived(formatado && texto === formatado.para ? formatado.de : texto);
	let mudou = $derived(aSalvar !== servidor);
	let colorir = $derived(pareceJson(texto) && texto.length <= TETO_REALCE_BYTES);

	/**
	 * O `\n` no fim existe para as duas camadas terminarem na mesma altura:
	 * o textarea reserva uma linha depois da última quebra, o `<pre>` não.
	 */
	let tokens = $derived(colorir ? realcarJson(`${texto}\n`) : []);

	let camada = $state<HTMLPreElement | null>(null);

	/** A camada de baixo acompanha a rolagem da de cima. */
	function rolar(evento: Event & { currentTarget: HTMLTextAreaElement }) {
		if (!camada) return;
		camada.scrollTop = evento.currentTarget.scrollTop;
		camada.scrollLeft = evento.currentTarget.scrollLeft;
	}
</script>

{#if binario}
	<p class="aviso">
		Valor binário: editar como texto corromperia o conteúdo. Use o painel de
		leitura para conferi-lo.
	</p>
{:else}
	<div class="editor">
		<label for="valor-chave">Valor</label>

		<div class="campo" class:colorido={colorir}>
			{#if colorir}
				<pre
					class="camada"
					bind:this={camada}
					aria-hidden="true"
					data-testid="json-realce">{#each tokens as token}{#if token.classe}<span
								class={token.classe}>{token.texto}</span>{:else}{token.texto}{/if}{/each}</pre>
			{/if}
			<textarea
				id="valor-chave"
				bind:value={texto}
				onscroll={rolar}
				spellcheck="false"
				autocapitalize="off"
				wrap="off"
				rows="18"
			></textarea>
		</div>

		{#if cortado}
			<p class="cortado" role="alert" data-testid="cortado">
				Mostrando só o começo deste valor ({numero(view.total_elements)} bytes no
				total). Para editar sem perder o resto, use “Carregar mais” até carregar
				o valor inteiro.
			</p>
		{:else if erroJson}
			<p class="erro-json" role="alert" data-testid="json-erro">
				{erroJson} Uma string no Redis não precisa ser JSON — dá para salvar assim
				mesmo.
			</p>
		{:else if pareceJson(texto) && !colorir}
			<p class="aviso">
				Valor grande demais para colorir enquanto se digita. O conteúdo e a
				validação continuam valendo.
			</p>
		{/if}

		<div class="acoes">
			{#if indentado !== null && indentado !== texto}
				<button
					type="button"
					onclick={() => {
						if (indentado === null) return;
						formatado = { de: texto, para: indentado };
						texto = indentado;
					}}
				>
					Formatar JSON
				</button>
			{/if}
			<button
				type="button"
				class="primary"
				disabled={busy || !mudou || cortado}
				onclick={() => onSubmit({ op: "set_string", value: textBytes(aSalvar) })}
			>
				Salvar
			</button>
		</div>
	</div>
{/if}

<style>
	.editor {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	label {
		color: var(--text-dim);
		font-size: 0.9rem;
	}

	.campo {
		position: relative;
		display: grid;
	}

	/*
	 * A regra compartilhada: TUDO que decide onde cada caractere cai mora
	 * aqui, uma vez, para as duas camadas. Separar qualquer uma destas
	 * linhas entre `.camada` e `textarea` desalinha o cursor do texto.
	 */
	.camada,
	.campo textarea {
		grid-area: 1 / 1;
		margin: 0;
		padding: 0.55rem 0.65rem;
		border: 1px solid var(--rule-strong);
		border-radius: var(--radius);
		font-family: var(--mono);
		font-size: 0.92em;
		line-height: 1.5;
		tab-size: 2;
		white-space: pre;
		overflow: auto;
		word-break: normal;
		overflow-wrap: normal;
	}

	.camada {
		background: var(--panel-dark);
		color: var(--text);
		pointer-events: none;
		border-color: transparent;
	}

	.campo textarea {
		background: transparent;
		color: var(--text);
		resize: vertical;
		min-height: 12rem;
	}

	/* Com a camada de cor atrás, o texto do textarea some e fica só o
	   cursor — e a seleção, que o navegador desenha por cima de todo jeito. */
	.colorido textarea {
		color: transparent;
		caret-color: var(--text);
	}

	.colorido textarea::selection {
		background: rgb(242 163 60 / 0.3);
	}

	/* Cada tipo com a sua cor, todas conferidas contra o fundo do painel.
	   A chave puxa para o âmbar da ação porque é por ela que se procura. */
	.camada :global(.chave) {
		color: #f2a33c;
	}

	.camada :global(.texto) {
		color: #9ecf8a;
	}

	.camada :global(.numero) {
		color: #7fb6e8;
	}

	.camada :global(.booleano) {
		color: #d08fd0;
	}

	.camada :global(.nulo) {
		color: var(--text-faint);
		font-style: italic;
	}

	.acoes {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
		margin-top: 0.35rem;
	}

	.aviso {
		margin: 0;
		color: var(--text-faint);
		font-size: 0.9rem;
	}

	.cortado,
	.erro-json {
		margin: 0;
		padding: 0.4rem 0.6rem;
		border: 1px solid #5e3a38;
		border-radius: var(--radius);
		background: #2e2326;
		color: #e49a94;
	}
</style>
