<script lang="ts">
	import { untrack } from "svelte";
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

	let texto = $state(
		untrack(() => (view.value.kind === "string" ? (view.value.data.text ?? "") : "")),
	);

	/** O conteúdo é JSON? Só então faz sentido oferecer para formatá-lo. */
	let json = $derived.by(() => {
		const limpo = texto.trim();
		if (!limpo.startsWith("{") && !limpo.startsWith("[")) return null;
		try {
			return JSON.stringify(JSON.parse(limpo), null, 2);
		} catch {
			return null;
		}
	});

	let mudou = $derived(texto !== (dado?.text ?? ""));
</script>

{#if binario}
	<p class="aviso">
		Valor binário: editar como texto corromperia o conteúdo. Use o painel de
		leitura para conferi-lo.
	</p>
{:else}
	<div class="editor">
		<label>
			<span>Valor</span>
			<textarea bind:value={texto} spellcheck="false" rows="14" class="mono"></textarea>
		</label>

		<div class="acoes">
			{#if json !== null && json !== texto}
				<button type="button" onclick={() => (texto = json ?? texto)}>
					Formatar JSON
				</button>
			{/if}
			<button
				type="button"
				class="primary"
				disabled={busy || !mudou}
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

	textarea {
		font-family: var(--mono);
		font-size: 0.92em;
		color: var(--text);
		background: var(--panel-dark);
		border: 1px solid var(--rule-strong);
		border-radius: var(--radius);
		padding: 0.55rem 0.65rem;
		resize: vertical;
		width: 100%;
	}

	.acoes {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
	}

	.aviso {
		margin: 0;
		color: var(--text-faint);
	}
</style>
