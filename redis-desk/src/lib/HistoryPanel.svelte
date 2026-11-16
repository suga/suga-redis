<script lang="ts">
	import { idioma, t } from "$lib/i18n.svelte";
	import { resumoDaOperacao } from "$lib/resumo";
	/**
	 * O histórico de mudanças do host, mais recente primeiro.
	 *
	 * Cada linha mostra o valor ANTES e DEPOIS lado a lado: é o que permite
	 * responder "o que exatamente mudou?" sem ter que confiar na memória de
	 * quem mexeu — inclusive quando quem mexeu foi o Claude.
	 */
	import ConfirmDialog from "$lib/ConfirmDialog.svelte";
	import { numero } from "$lib/formato";
	import {
		errorMessage,
		listHistory,
		rollback,
		type HistoryEntry,
	} from "$lib/api";

	interface Props {
		hostId: string;
		/** Chamado depois de uma volta atrás aplicada, para a tela recarregar. */
		onRolledBack?: () => void;
	}

	let { hostId, onRolledBack }: Props = $props();

	/** Quantas entradas a tela pede por vez. */
	const LIMITE = 200;

	let entradas = $state<HistoryEntry[]>([]);
	let carregando = $state(false);
	let erro = $state("");
	let pendente = $state<{ id: number; summary: string } | null>(null);
	let desfazendo = $state(false);

	async function carregar() {
		carregando = true;
		erro = "";
		try {
			entradas = await listHistory(hostId, LIMITE);
		} catch (err) {
			entradas = [];
			erro = errorMessage(err);
		} finally {
			carregando = false;
		}
	}

	$effect(() => {
		carregar();
	});

	async function desfazer(id: number, confirmado: boolean) {
		desfazendo = true;
		erro = "";
		try {
			const resultado = await rollback(id, confirmado);
			if (resultado.outcome === "needs_confirmation") {
				pendente = { id, summary: resultado.summary };
			} else {
				pendente = null;
				await carregar();
				onRolledBack?.();
			}
		} catch (err) {
			pendente = null;
			erro = errorMessage(err);
		} finally {
			desfazendo = false;
		}
	}

	/**
	 * O texto de um instantâneo guardado em `before_json`/`after_json`.
	 *
	 * `null` quer dizer que a chave NÃO EXISTIA — e isso é diferente de uma
	 * chave vazia, então vira uma frase em vez de um espaço em branco que a
	 * pessoa teria que interpretar.
	 */
	function resumoDoValor(json: string | null): string {
		if (json === null) return "a chave não existia";
		try {
			const snapshot = JSON.parse(json) as {
				kind: string;
				elements: {
					field: string | null;
					value: { text: string | null; base64: string | null };
					score: number | null;
				}[];
			};
			if (snapshot.elements.length === 0) return "(vazio)";

			const linhas = snapshot.elements.slice(0, 12).map((el) => {
				const valor = el.value.text ?? "(binário)";
				if (el.field !== null) return `${el.field}: ${valor}`;
				if (el.score !== null) return `${valor} (${el.score})`;
				return valor;
			});
			const restantes = snapshot.elements.length - linhas.length;
			if (restantes > 0) linhas.push(`… e mais ${numero(restantes)}`);
			return linhas.join("\n");
		} catch {
			// Um JSON ilegível aqui não pode derrubar a aba inteira: a entrada
			// ainda tem resumo, chave e data, que já valem alguma coisa.
			return "(não foi possível ler a cópia)";
		}
	}

	function quando(ms: number): string {
		return new Date(ms).toLocaleString(idioma());
	}
</script>

<section>
	{#if erro}
		<p class="erro" role="alert">{erro}</p>
	{/if}

	{#if carregando && entradas.length === 0}
		<p class="aviso">{t("historico.lendo")}</p>
	{:else if entradas.length === 0}
		<p class="vazio" data-testid="vazio">
			{t("historico.vazio")}
		</p>
	{:else}
		<ul>
			{#each entradas as entrada (entrada.id)}
				<li data-testid="entrada" class:revertida={entrada.rolled_back}>
					<div class="cabeca">
						<span class="resumo">
							{entrada.op
								? resumoDaOperacao(entrada.key, entrada.op)
								: entrada.summary}
						</span>
						{#if entrada.rolled_back}
							<span class="marca">{t("historico.jaRevertida")}</span>
						{:else}
							<button
								disabled={desfazendo}
								onclick={() => desfazer(entrada.id, false)}
							>
								{t("historico.voltarAtras")}
							</button>
						{/if}
					</div>

					<div class="meta">
						<span class="chave mono">{entrada.key}</span>
						<span>banco {entrada.db}</span>
						<span data-testid="autor">
							{entrada.actor === "llm" ? t("autor.claude") : t("autor.voce")}
						</span>
						<span>{quando(entrada.at)}</span>
					</div>

					<div class="lados">
						<div class="lado">
							<p class="rotulo">{t("historico.antes")}</p>
							<pre class="mono" data-testid="antes">{resumoDoValor(
									entrada.before_json,
								)}</pre>
						</div>
						<div class="lado">
							<p class="rotulo">{t("historico.depois")}</p>
							<pre class="mono" data-testid="depois">{resumoDoValor(
									entrada.after_json,
								)}</pre>
						</div>
					</div>
				</li>
			{/each}
		</ul>
	{/if}
</section>

{#if pendente}
	<ConfirmDialog
		summary={pendente.summary}
		onConfirm={() => pendente && desfazer(pendente.id, true)}
		onCancel={() => (pendente = null)}
		busy={desfazendo}
	/>
{/if}

<style>
	section {
		min-width: 0;
	}

	ul {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.9rem;
	}

	li {
		padding-bottom: 0.9rem;
		border-bottom: 1px solid var(--rule);
	}

	.revertida {
		opacity: 0.62;
	}

	.cabeca {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 1rem;
	}

	.resumo {
		font-weight: 500;
	}

	.cabeca button {
		padding: 0.1rem 0.5rem;
		font-size: 0.88rem;
		color: var(--text-dim);
		white-space: nowrap;
	}

	.marca {
		color: var(--text-faint);
		font-size: 0.88rem;
		white-space: nowrap;
	}

	.meta {
		display: flex;
		flex-wrap: wrap;
		gap: 0.9rem;
		margin-top: 0.2rem;
		color: var(--text-faint);
		font-size: 0.88rem;
	}

	.chave {
		color: var(--text-dim);
		word-break: break-all;
	}

	.lados {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 0.6rem;
		margin-top: 0.55rem;
	}

	.rotulo {
		margin: 0 0 0.2rem;
		color: var(--text-faint);
		font-size: 0.85rem;
	}

	pre {
		margin: 0;
		padding: 0.45rem 0.55rem;
		max-height: 11rem;
		overflow: auto;
		background: var(--panel-dark);
		border: 1px solid var(--rule);
		border-radius: var(--radius);
		white-space: pre-wrap;
		word-break: break-word;
		font-size: 0.88em;
	}

	.aviso,
	.vazio {
		margin: 0;
		color: var(--text-faint);
	}

	.erro {
		margin: 0 0 0.8rem;
		padding: 0.4rem 0.6rem;
		border: 1px solid #5e3a38;
		border-radius: var(--radius);
		background: #2e2326;
		color: #e49a94;
	}

	@media (max-width: 44rem) {
		.lados {
			grid-template-columns: 1fr;
		}
	}
</style>
