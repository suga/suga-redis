<script lang="ts">
	import ErroTecnico from "$lib/ErroTecnico.svelte";
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
		type FailedKey,
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
	/** Qual operação produziu o erro que está na tela. */
	let assuntoErro = $state<"erro.aoLerHistorico" | "erro.aoDesfazer">(
		"erro.aoLerHistorico",
	);
	let pendente = $state<{ id: number; summary: string } | null>(null);
	let desfazendo = $state(false);
	/**
	 * A última volta atrás que não trouxe tudo de volta.
	 *
	 * Uma exclusão em massa repõe retrato a retrato, e um retrato pode
	 * falhar sozinho — a chave voltou a existir no meio tempo, mudou de
	 * tipo, expirou. Dizer só "pronto" esconderia exatamente o que ficou
	 * para trás, que é a única parte que ainda pede ação de alguém.
	 */
	let parcial = $state<{ id: number; restored: number; failed: FailedKey[] } | null>(null);

	async function carregar() {
		carregando = true;
		erro = "";
		try {
			entradas = await listHistory(hostId, LIMITE);
		} catch (err) {
			entradas = [];
			assuntoErro = "erro.aoLerHistorico";
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
		parcial = null;
		try {
			const resultado = await rollback(id, confirmado);
			if (resultado.outcome === "needs_confirmation") {
				pendente = { id, summary: resultado.summary };
			} else {
				pendente = null;
				if (resultado.outcome === "restored" && resultado.failed.length > 0) {
					parcial = { id, restored: resultado.restored, failed: resultado.failed };
				}
				await carregar();
				onRolledBack?.();
			}
		} catch (err) {
			pendente = null;
			assuntoErro = "erro.aoDesfazer";
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
		if (json === null) return t("historico.chaveNaoExistia");
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

	/**
	 * Onde está a cópia de segurança de uma exclusão em massa — `null`
	 * quando a entrada não é uma, e `{ backup: "none" }` quando é uma que
	 * foi feita sem cópia.
	 *
	 * É esta variante, e só ela, que diz se há o que restaurar. A entrada
	 * guarda `before_json` vazio nos dois casos de cópia em arquivo e sem
	 * cópia, então olhar para ele confundiria os dois.
	 */
	function copiaDa(entrada: HistoryEntry) {
		return entrada.op?.op === "delete_prefix" ? entrada.op.backup : null;
	}
</script>

<section>
	{#if erro}
		<ErroTecnico assunto={assuntoErro} detalhe={erro} />
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
				{@const copia = copiaDa(entrada)}
				<li data-testid="entrada" class:revertida={entrada.rolled_back}>
					<div class="cabeca">
						<span class="resumo">
							{entrada.op
								? resumoDaOperacao(entrada.key, entrada.op)
								: entrada.summary}
						</span>
						{#if entrada.rolled_back}
							<span class="marca">{t("historico.jaRevertida")}</span>
						{:else if copia?.backup === "none"}
							<!--
								Nada de botão cinza aqui. Um botão desabilitado promete
								que um dia acende, e este nunca acende: a cópia não
								existe, e nenhuma espera vai criá-la. A frase ao lado
								diz isso em vez de deixar a pessoa procurar o motivo.
							-->
							<span class="marca" data-testid="sem-volta">{t("historico.semCopia")}</span>
						{:else}
							<button
								disabled={desfazendo}
								onclick={() => desfazer(entrada.id, false)}
							>
								{t("historico.voltarAtras")}
							</button>
						{/if}
					</div>

					{#if copia?.backup === "file"}
						<p class="copia mono">{t("historico.copiaEmArquivo", { caminho: copia.path })}</p>
					{/if}

					{#if parcial && parcial.id === entrada.id}
						<div class="parcial" data-testid="volta-parcial" role="status">
							<p class="parcial-linha">
								{t("historico.voltouParcial", { n: numero(parcial.restored) })}
							</p>
							<ul class="faltaram" aria-label={t("historico.naoVoltaram")}>
								{#each parcial.failed as falha (falha.key)}
									<li><span class="mono">{falha.key}</span> — {falha.reason}</li>
								{/each}
							</ul>
						</div>
					{/if}

					<div class="meta">
						<span class="chave mono">{entrada.key}</span>
						<span>{t("banco.rotulo", { n: entrada.db })}</span>
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

	/* O caminho do arquivo e o aviso de parcial são prosa de corpo, não
	   sussurro: os dois dizem onde a pessoa precisa ir olhar em seguida. */
	.copia {
		margin: 0.3rem 0 0;
		color: var(--text-dim);
		font-size: 0.88rem;
		word-break: break-all;
	}

	/* Mesmo degrau do aviso que contradiz uma crença no diálogo de apagar:
	   peso e um traço ao lado, nunca cor — o vermelho `--prod` significa só
	   o ambiente. Quem acabou de clicar em "Voltar atrás" acredita que
	   voltou tudo, e esta é a frase que diz que não. */
	.parcial {
		margin-top: 0.45rem;
		padding-left: 0.6rem;
		border-left: 2px solid var(--text-dim);
	}

	.parcial-linha {
		margin: 0;
		font-weight: 600;
	}

	.faltaram {
		margin: 0.25rem 0 0;
		padding-left: 1.1rem;
		color: var(--text-dim);
		font-size: 0.88rem;
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


	@media (max-width: 44rem) {
		.lados {
			grid-template-columns: 1fr;
		}
	}
</style>
