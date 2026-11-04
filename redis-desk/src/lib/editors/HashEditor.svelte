<script lang="ts">
	import { untrack } from "svelte";
	import { textBytes, type Bytes, type ValueView, type WriteOp } from "$lib/api";

	interface Props {
		view: ValueView;
		onSubmit: (op: WriteOp) => void;
		busy?: boolean;
	}

	let { view, onSubmit, busy = false }: Props = $props();

	interface Linha {
		field: string;
		original: string | null;
		texto: string;
	}

	/**
	 * Cada campo guarda o que veio do Redis (`original`) ao lado do que a
	 * pessoa digitou: é a comparação entre os dois que diz quais campos
	 * mudaram, e só esses viram escrita. Mandar o hash inteiro a cada
	 * salvamento sobrescreveria campos que outra pessoa mexeu enquanto esta
	 * tela estava aberta.
	 */
	function linhasIniciais(): Linha[] {
		const dados = view.value.kind === "hash" ? view.value.data : [];
		return dados.map(([field, bytes]: [string, Bytes]) => ({
			field,
			original: bytes.text,
			texto: bytes.text ?? "",
		}));
	}

	let linhas = $state(untrack(linhasIniciais));
	let novoCampo = $state("");
	let novoValor = $state("");
	let erroNovo = $state("");

	function adicionar() {
		const campo = novoCampo.trim();
		if (!campo) {
			erroNovo = "Informe o nome do campo.";
			return;
		}
		erroNovo = "";
		onSubmit({ op: "set_hash_field", field: campo, value: textBytes(novoValor) });
		novoCampo = "";
		novoValor = "";
	}
</script>

<div class="editor">
	<table>
		<tbody>
			{#each linhas as linha (linha.field)}
				<tr>
					<th class="mono" scope="row">{linha.field}</th>
					<td>
						{#if linha.original === null}
							<span class="binario">Valor binário: não editável como texto</span>
						{:else}
							<input
								class="mono"
								bind:value={linha.texto}
								aria-label="Valor do campo {linha.field}"
							/>
						{/if}
					</td>
					<td class="acoes-linha">
						<button
							type="button"
							aria-label="Salvar campo {linha.field}"
							disabled={busy || linha.original === null || linha.texto === linha.original}
							onclick={() =>
								onSubmit({
									op: "set_hash_field",
									field: linha.field,
									value: textBytes(linha.texto),
								})}
						>
							Salvar
						</button>
						<button
							type="button"
							class="danger"
							aria-label="Remover campo {linha.field}"
							disabled={busy}
							onclick={() => onSubmit({ op: "delete_hash_field", field: linha.field })}
						>
							Remover
						</button>
					</td>
				</tr>
			{/each}
		</tbody>
	</table>

	<div class="novo">
		<input bind:value={novoCampo} class="mono" aria-label="Novo campo" placeholder="campo" />
		<input bind:value={novoValor} class="mono" aria-label="Valor do novo campo" placeholder="valor" />
		<button type="button" onclick={adicionar} disabled={busy}>Adicionar campo</button>
	</div>

	{#if erroNovo}
		<p class="erro" role="alert">{erroNovo}</p>
	{/if}
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
		white-space: nowrap;
		color: var(--text-dim);
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
		gap: 0.5rem;
		align-items: center;
	}

	.novo button {
		white-space: nowrap;
	}

	.binario {
		color: var(--text-faint);
		font-style: italic;
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
