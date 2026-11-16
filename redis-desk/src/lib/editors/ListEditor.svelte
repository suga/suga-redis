<script lang="ts">
	import { t } from "$lib/i18n.svelte";
	import { untrack } from "svelte";
	import { textBytes, type Bytes, type ValueView, type WriteOp } from "$lib/api";

	interface Props {
		view: ValueView;
		onSubmit: (op: WriteOp) => void;
		busy?: boolean;
	}

	let { view, onSubmit, busy = false }: Props = $props();

	interface Item {
		indice: number;
		original: string | null;
		texto: string;
	}

	function itensIniciais(): Item[] {
		const dados = view.value.kind === "list" ? view.value.data : [];
		return dados.map((bytes: Bytes, indice: number) => ({
			indice,
			original: bytes.text,
			texto: bytes.text ?? "",
		}));
	}

	let itens = $state(untrack(itensIniciais));
	let novo = $state("");
	let naFrente = $state(false);
</script>

<div class="editor">
	<table>
		<tbody>
			{#each itens as item (item.indice)}
				<tr>
					<th class="mono" scope="row">{item.indice}</th>
					<td>
						{#if item.original === null}
							<span class="binario">{t("binario.naoEditavel")}</span>
						{:else}
							<input
								class="mono"
								bind:value={item.texto}
								aria-label={t("lista.valorDoItem", { indice: item.indice })}
							/>
						{/if}
					</td>
					<td class="acoes-linha">
						<button
							type="button"
							aria-label={t("lista.salvarItem", { indice: item.indice })}
							disabled={busy || item.original === null || item.texto === item.original}
							onclick={() =>
								onSubmit({
									op: "set_list_item",
									index: item.indice,
									value: textBytes(item.texto),
								})}
						>
							Salvar
						</button>
						<button
							type="button"
							class="danger"
							aria-label={t("lista.removerItem", { indice: item.indice })}
							disabled={busy}
							onclick={() => onSubmit({ op: "remove_list_item", index: item.indice })}
						>
							Remover
						</button>
					</td>
				</tr>
			{/each}
		</tbody>
	</table>

	<div class="novo">
		<input bind:value={novo} class="mono" aria-label={t("lista.novoItem")} placeholder={t("hash.placeholderValor")} />
		<label class="ponta">
			<input type="checkbox" bind:checked={naFrente} />
			<span>{t("lista.noComeco")}</span>
		</label>
		<button
			type="button"
			disabled={busy}
			onclick={() => {
				onSubmit({ op: "push_list_item", value: textBytes(novo), front: naFrente });
				novo = "";
			}}
		>
			{t("lista.adicionarItem")}
		</button>
	</div>
</div>

<style>
	.editor {
		display: flex;
		flex-direction: column;
		gap: 0.7rem;
	}

	table {
		width: 100%;
		border-collapse: collapse;
	}

	tr {
		border-bottom: 1px solid var(--rule);
	}

	th,
	td {
		padding: 0.3rem 0.5rem 0.3rem 0;
		text-align: left;
		vertical-align: middle;
		font-weight: 400;
	}

	th {
		width: 1%;
		text-align: right;
		color: var(--text-faint);
		padding-right: 0.7rem;
	}

	.acoes-linha {
		width: 1%;
		white-space: nowrap;
		padding-left: 0.5rem;
	}

	.acoes-linha button {
		padding: 0.1rem 0.45rem;
		font-size: 0.88rem;
		color: var(--text-dim);
	}

	.novo {
		display: flex;
		gap: 0.6rem;
		align-items: center;
	}

	.ponta {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		white-space: nowrap;
		color: var(--text-dim);
		font-size: 0.9rem;
	}

	.novo button {
		white-space: nowrap;
	}

	.binario {
		color: var(--text-faint);
		font-style: italic;
	}
</style>
