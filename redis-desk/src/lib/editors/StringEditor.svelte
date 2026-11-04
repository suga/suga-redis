<script lang="ts">
	import { untrack } from "svelte";
	import { formatarJson, pareceJson, realcarJson, validarJson } from "$lib/json";
	import { textBytes, type ValueView, type WriteOp } from "$lib/api";

	interface Props {
		view: ValueView;
		onSubmit: (op: WriteOp) => void;
		busy?: boolean;
	}

	let { view, onSubmit, busy = false }: Props = $props();

	let dado = $derived(view.value.kind === "string" ? view.value.data : null);

	/**
	 * Valor que não é UTF-8 não entra num campo de texto: o que voltasse de
	 * lá já não seriam os mesmos bytes. A chave continua legível no painel,
	 * só não editável por aqui.
	 */
	let binario = $derived(dado !== null && dado.text === null);

	const inicial = untrack(() =>
		view.value.kind === "string" ? (view.value.data.text ?? "") : "",
	);
	let texto = $state(inicial);

	/**
	 * Um valor JSON abre para LER, colorido, porque é assim que se confere
	 * um valor de produção; editar é um passo deliberado a partir dali. Um
	 * valor que não é JSON abre direto no campo de texto: não há nada a
	 * colorir, e um clique a mais só atrapalharia.
	 */
	let modo = $state<"realce" | "texto">(
		untrack(() => (pareceJson(inicial) && validarJson(inicial) === null ? "realce" : "texto")),
	);

	/** Há algo a colorir? Só então o alternador faz sentido. */
	let ehJson = $derived(pareceJson(texto));
	let erroJson = $derived(validarJson(texto));
	let indentado = $derived(formatarJson(texto));
	let mudou = $derived(texto !== (dado?.text ?? ""));
</script>

{#if binario}
	<p class="aviso">
		Valor binário: editar como texto corromperia o conteúdo. Use o painel de
		leitura para conferi-lo.
	</p>
{:else}
	<div class="editor">
		{#if ehJson}
			<!-- Sempre no TOPO e nos dois modos: um valor de dezenas de
			     kilobytes rola muito, e um botão depois do conteúdo só é
			     achado por quem já percorreu tudo. -->
			<div class="modo" data-testid="modo-valor" role="group" aria-label="Como ver o valor">
				<button
					type="button"
					class:ativo={modo === "realce"}
					aria-pressed={modo === "realce"}
					onclick={() => (modo = "realce")}
				>
					Colorido
				</button>
				<button
					type="button"
					class:ativo={modo === "texto"}
					aria-pressed={modo === "texto"}
					onclick={() => (modo = "texto")}
				>
					Editar
				</button>
			</div>
		{/if}

		{#if modo === "realce"}
			<pre class="mono realce" data-testid="json-realce">{#each realcarJson(indentado ?? texto) as token}{#if token.classe}<span
						class={token.classe}>{token.texto}</span>{:else}{token.texto}{/if}{/each}</pre>
		{:else}
			<label>
				<span>Valor</span>
				<textarea bind:value={texto} spellcheck="false" rows="18" class="mono"></textarea>
			</label>

			{#if erroJson}
				<p class="erro-json" role="alert" data-testid="json-erro">{erroJson}</p>
			{/if}
		{/if}

		<div class="acoes">
			{#if modo === "texto" && indentado !== null && indentado !== texto}
				<button type="button" onclick={() => (texto = indentado ?? texto)}>
					Formatar JSON
				</button>
			{/if}
			<button
				type="button"
				class="primary"
				disabled={busy || !mudou || erroJson !== null || modo === "realce"}
				onclick={() => onSubmit({ op: "set_string", value: textBytes(texto) })}
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
		gap: 0.6rem;
	}

	label {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	label > span {
		color: var(--text-dim);
		font-size: 0.9rem;
	}

	textarea,
	.realce {
		font-family: var(--mono);
		font-size: 0.92em;
		color: var(--text);
		background: var(--panel-dark);
		border: 1px solid var(--rule-strong);
		border-radius: var(--radius);
		padding: 0.55rem 0.65rem;
		width: 100%;
	}

	textarea {
		resize: vertical;
	}

	.realce {
		margin: 0;
		max-height: 32rem;
		overflow: auto;
		white-space: pre;
	}

	/* Cada tipo com a sua cor, todas conferidas contra o fundo do painel.
	   A chave puxa para o âmbar da ação porque é por ela que se procura. */
	.realce .chave {
		color: #f2a33c;
	}

	.realce .texto {
		color: #9ecf8a;
	}

	.realce .numero {
		color: #7fb6e8;
	}

	.realce .booleano {
		color: #d08fd0;
	}

	.realce .nulo {
		color: var(--text-faint);
		font-style: italic;
	}

	.acoes {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
	}

	/* Controle segmentado: os dois estados ficam à vista o tempo todo, para
	   a pessoa saber que o outro existe sem precisar descobrir. */
	.modo {
		display: flex;
		align-self: flex-start;
		gap: 0;
	}

	.modo button {
		font-size: 0.88rem;
		color: var(--text-faint);
		border-radius: 0;
	}

	.modo button:first-child {
		border-radius: var(--radius) 0 0 var(--radius);
	}

	.modo button:last-child {
		border-radius: 0 var(--radius) var(--radius) 0;
		border-left-width: 0;
	}

	.modo .ativo {
		background: var(--panel-dark);
		color: var(--text);
		border-color: var(--text-faint);
	}

	.aviso {
		margin: 0;
		color: var(--text-faint);
	}

	.erro-json {
		margin: 0;
		padding: 0.4rem 0.6rem;
		border: 1px solid #5e3a38;
		border-radius: var(--radius);
		background: #2e2326;
		color: #e49a94;
	}
</style>
