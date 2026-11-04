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
	import { formatarPrazo } from "$lib/formato";
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

	// ------------------------------------------------------- prazo

	/** Unidades oferecidas, da mais curta para a mais longa. */
	const UNIDADES = [
		{ id: "s", label: "segundos", ms: 1000 },
		{ id: "min", label: "minutos", ms: 60_000 },
		{ id: "h", label: "horas", ms: 3_600_000 },
		{ id: "d", label: "dias", ms: 86_400_000 },
	] as const;

	// `bind:value` num input numérico entrega número (ou `null` quando o
	// campo está vazio) — não texto.
	let quanto = $state<number | null>(null);
	let unidade = $state<(typeof UNIDADES)[number]["id"]>("min");

	/**
	 * O prazo pedido em milissegundos, ou `null` se o que está digitado não
	 * dá um prazo. Zero não conta: no Redis um TTL de zero apaga a chave na
	 * hora, e quem digita "0" num campo de prazo quase nunca quer isso —
	 * quem quer apagar tem o botão de apagar logo ali.
	 */
	let prazoMs = $derived.by(() => {
		if (quanto === null || !Number.isFinite(quanto) || quanto <= 0) return null;
		const fator = UNIDADES.find((u) => u.id === unidade)?.ms ?? 1000;
		return Math.round(quanto * fator);
	});
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
		<div class="prazo">
			<p class="prazo-atual">
				{#if view.ttl_ms !== null}
					Expira em <span class="mono" data-testid="prazo-atual"
						>{formatarPrazo(view.ttl_ms)}</span
					>
				{:else}
					Esta chave não expira.
				{/if}
			</p>

			<div class="prazo-campos">
				<label class="quanto">
					<span>Novo prazo</span>
					<input type="number" min="1" bind:value={quanto} class="mono" />
				</label>
				<label class="unidade">
					<span>Unidade do prazo</span>
					<select bind:value={unidade}>
						{#each UNIDADES as u (u.id)}
							<option value={u.id}>{u.label}</option>
						{/each}
					</select>
				</label>
				<button
					type="button"
					disabled={salvando || prazoMs === null}
					onclick={() => prazoMs !== null && enviar({ op: "set_ttl", ttl_ms: prazoMs }, false)}
				>
					Definir prazo
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

		<button
			type="button"
			class="danger apagar"
			disabled={salvando}
			onclick={() => enviar({ op: "delete_key" }, false)}
		>
			Apagar a chave
		</button>
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
		flex-direction: column;
		gap: 0.7rem;
		padding-top: 0.7rem;
		border-top: 1px solid var(--rule);
	}

	.comuns button {
		font-size: 0.9rem;
		color: var(--text-dim);
	}

	.prazo-atual {
		margin: 0 0 0.4rem;
		color: var(--text-dim);
		font-size: 0.9rem;
	}

	.prazo-campos {
		display: flex;
		flex-wrap: wrap;
		align-items: flex-end;
		gap: 0.5rem;
	}

	.prazo-campos label {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
	}

	.prazo-campos label > span {
		color: var(--text-faint);
		font-size: 0.85rem;
	}

	.quanto {
		width: 7rem;
	}

	.unidade {
		width: 9rem;
	}

	/* Apagar fica numa linha só dele, longe dos campos de prazo: são ações
	   de peso muito diferente para dividirem a mesma fileira. */
	.apagar {
		align-self: flex-start;
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
