<script lang="ts">
	/**
	 * Põe o editor do tipo na tela e cuida de TUDO que envolve escrever.
	 *
	 * Os editores por tipo não falam com o backend: eles montam um `WriteOp`
	 * e entregam aqui. Assim a trava de produção, o diálogo de confirmar e o
	 * tratamento de erro existem UMA vez — um editor novo não tem como
	 * esquecer de pedir confirmação, porque não é ele quem pede.
	 */
	import ConfirmDialog from "$lib/ConfirmDialog.svelte";
	import HashEditor from "$lib/editors/HashEditor.svelte";
	import ListEditor from "$lib/editors/ListEditor.svelte";
	import StringEditor from "$lib/editors/StringEditor.svelte";
	import ZSetEditor from "$lib/editors/ZSetEditor.svelte";
	import { errorMessage, writeValue, type ValueView, type WriteOp } from "$lib/api";

	interface Props {
		hostId: string;
		db: number;
		view: ValueView;
		/** Chamado depois de uma escrita aplicada, para a tela reler a chave. */
		onWritten: () => void;
	}

	let { hostId, db, view, onWritten }: Props = $props();

	/** A operação que o backend recusou até alguém confirmar. */
	let pendente = $state<{ op: WriteOp; summary: string } | null>(null);
	let salvando = $state(false);
	let erro = $state("");

	async function enviar(op: WriteOp, confirmado: boolean) {
		salvando = true;
		erro = "";
		try {
			const resultado = await writeValue(hostId, db, view.key, op, confirmado);
			if (resultado.outcome === "needs_confirmation") {
				pendente = { op, summary: resultado.summary };
			} else {
				pendente = null;
				onWritten();
			}
		} catch (err) {
			// O diálogo sai da frente, mas o que a pessoa digitou fica: o
			// editor é o único lugar onde aquela edição existe, e limpá-la
			// por causa de uma queda de rede perderia o trabalho dela.
			pendente = null;
			erro = errorMessage(err);
		} finally {
			salvando = false;
		}
	}

	function confirmar() {
		if (pendente) enviar(pendente.op, true);
	}
</script>

<div class="editor">
	{#if erro}
		<p class="erro" role="alert">{erro}</p>
	{/if}

	{#if view.kind === "string"}
		<StringEditor {view} onSubmit={(op) => enviar(op, false)} busy={salvando} />
	{:else if view.kind === "hash"}
		<HashEditor {view} onSubmit={(op) => enviar(op, false)} busy={salvando} />
	{:else if view.kind === "list"}
		<ListEditor {view} onSubmit={(op) => enviar(op, false)} busy={salvando} />
	{:else if view.kind === "zset"}
		<ZSetEditor {view} onSubmit={(op) => enviar(op, false)} busy={salvando} />
	{/if}

	<div class="comuns">
		<button
			type="button"
			class="danger"
			disabled={salvando}
			onclick={() => enviar({ op: "delete_key" }, false)}
		>
			Apagar a chave
		</button>
		{#if view.ttl_ms !== null}
			<button
				type="button"
				disabled={salvando}
				onclick={() => enviar({ op: "set_ttl", ttl_ms: null }, false)}
			>
				Tirar o prazo
			</button>
		{/if}
	</div>
</div>

{#if pendente}
	<ConfirmDialog
		summary={pendente.summary}
		onConfirm={confirmar}
		onCancel={() => (pendente = null)}
		busy={salvando}
	/>
{/if}

<style>
	.editor {
		display: flex;
		flex-direction: column;
		gap: 0.8rem;
	}

	.comuns {
		display: flex;
		gap: 0.5rem;
		padding-top: 0.7rem;
		border-top: 1px solid var(--rule);
	}

	.comuns button {
		font-size: 0.9rem;
		color: var(--text-dim);
	}

	.erro {
		margin: 0;
		padding: 0.4rem 0.6rem;
		border: 1px solid #5e3a38;
		border-radius: var(--radius);
		background: #2e2326;
		color: #e49a94;
	}
</style>
