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

	interface Membro {
		nome: string;
		binario: boolean;
		original: number;
		score: number;
	}

	/**
	 * Só a pontuação é editável. Trocar o TEXTO de um membro não é uma
	 * mudança: no sorted set o membro é a identidade, então renomear seria
	 * remover um e inserir outro — duas escritas, e portanto um estado
	 * intermediário em que nenhum dos dois existe direito. Quem quer isso
	 * remove e adiciona, vendo cada passo.
	 */
	function membrosIniciais(): Membro[] {
		const dados = view.value.kind === "zset" ? view.value.data : [];
		return dados.map(([bytes, score]: [Bytes, number]) => ({
			nome: bytes.text ?? "(binário)",
			binario: bytes.text === null,
			original: score,
			score,
		}));
	}

	let membros = $state(untrack(membrosIniciais));
	let novoMembro = $state("");
	let novaPontuacao = $state(0);
</script>

<div class="editor">
	<table>
		<tbody>
			{#each membros as membro (membro.nome)}
				<tr>
					<th class="mono" scope="row" class:binario={membro.binario}>{membro.nome}</th>
					<td class="pontuacao">
						<input
							type="number"
							step="any"
							class="mono"
							bind:value={membro.score}
							disabled={membro.binario}
							aria-label={t("zset.pontuacaoDe", { membro: membro.nome })}
						/>
					</td>
					<td class="acoes-linha">
						<button
							type="button"
							aria-label={t("zset.salvarMembro", { membro: membro.nome })}
							disabled={busy || membro.binario || membro.score === membro.original}
							onclick={() =>
								onSubmit({
									op: "set_zset_member",
									member: textBytes(membro.nome),
									score: membro.score,
								})}
						>
							Salvar
						</button>
						<button
							type="button"
							class="danger"
							aria-label={t("zset.removerMembro", { membro: membro.nome })}
							disabled={busy || membro.binario}
							onclick={() =>
								onSubmit({ op: "remove_zset_member", member: textBytes(membro.nome) })}
						>
							Remover
						</button>
					</td>
				</tr>
			{/each}
		</tbody>
	</table>

	<div class="novo">
		<input bind:value={novoMembro} class="mono" aria-label={t("zset.novoMembro")} placeholder={t("zset.placeholderMembro")} />
		<input
			type="number"
			step="any"
			bind:value={novaPontuacao}
			class="mono pontuacao-nova"
			aria-label={t("zset.pontuacaoDoNovo")}
		/>
		<button
			type="button"
			disabled={busy || !novoMembro.trim()}
			onclick={() => {
				onSubmit({
					op: "set_zset_member",
					member: textBytes(novoMembro),
					score: novaPontuacao,
				});
				novoMembro = "";
				novaPontuacao = 0;
			}}
		>
			{t("zset.adicionarMembro")}
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
		color: var(--text-dim);
		padding-right: 0.7rem;
		word-break: break-all;
	}

	.pontuacao {
		width: 8rem;
	}

	.pontuacao-nova {
		width: 8rem;
		flex: none;
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
</style>
