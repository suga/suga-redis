<script lang="ts">
	/**
	 * O convite para atualizar, no canto da tela.
	 *
	 * Não é um diálogo modal de propósito. Ele aparece sozinho ao abrir o
	 * app, e quem está no meio de uma edição em produção não pode ser
	 * interrompido por uma caixa que toma a tela e o foco. Daqui ele espera:
	 * atualizar é escolha, e "depois" é uma resposta legítima.
	 *
	 * Some de vez quando a pessoa dispensa — voltar a perguntar na mesma
	 * sessão seria insistência, não aviso.
	 */
	import { instalar, tamanho, type Achado, type Progresso } from "$lib/atualizacao";
	import { t } from "$lib/i18n.svelte";

	interface Props {
		achado: Achado;
		/** A versão que está rodando agora, para a frase dizer de onde para onde. */
		atual: string;
		onDispensar: () => void;
	}

	let { achado, atual, onDispensar }: Props = $props();

	let baixando = $state(false);
	let progresso = $state<Progresso | null>(null);
	let erro = $state("");

	async function atualizar() {
		baixando = true;
		erro = "";
		try {
			await instalar(achado, (p) => (progresso = p));
		} catch (falha) {
			// O app não se reabre, e a versão velha continua inteira no
			// lugar: o plugin só troca o aplicativo depois de verificar a
			// assinatura do pacote. Falhar aqui é ficar como estava.
			erro = falha instanceof Error ? falha.message : String(falha);
			baixando = false;
		}
	}

	const pct = $derived(
		progresso?.fracao === null || progresso?.fracao === undefined
			? null
			: Math.round(progresso.fracao * 100),
	);
</script>

<div class="aviso" role="status" aria-live="polite">
	{#if erro}
		<p class="titulo">{t("atualizacao.falhou")}</p>
		<p class="motivo">{erro}</p>
		<p class="tranquilize">{t("atualizacao.continuaInstalada", { versao: atual })}</p>
		<div class="acoes">
			<button type="button" onclick={onDispensar}>{t("acao.fechar")}</button>
			<button type="button" class="primary" onclick={atualizar}>{t("acao.tentarDeNovo")}</button>
		</div>
	{:else if baixando}
		<p class="titulo">{t("atualizacao.baixando", { versao: achado.versao })}</p>
		{#if pct !== null}
			<div class="barra">
				<div class="cheio" style="transform: scaleX({pct / 100})"></div>
			</div>
			<p class="progresso">{pct}%</p>
		{:else if progresso}
			<!-- Sem Content-Length não existe porcentagem honesta; mostrar
			     quanto já veio é o que dá para afirmar de verdade. -->
			<p class="progresso">{t("atualizacao.baixados", { tamanho: tamanho(progresso.baixado) })}</p>
		{:else}
			<p class="progresso">{t("atualizacao.comecando")}</p>
		{/if}
		<p class="tranquilize">{t("atualizacao.reabreSozinho")}</p>
	{:else}
		<p class="titulo">{t("atualizacao.disponivel", { versao: achado.versao })}</p>
		<p class="motivo">{t("atualizacao.vocEstaNa", { versao: atual })}</p>
		{#if achado.notas}
			<p class="notas">{achado.notas}</p>
		{/if}
		<div class="acoes">
			<button type="button" onclick={onDispensar}>{t("acao.agoraNao")}</button>
			<button type="button" class="primary" onclick={atualizar}>{t("acao.atualizar")}</button>
		</div>
	{/if}
</div>

<style>
	/* Ancorado no canto, acima de tudo menos dos diálogos de verdade
	   (`z-index: 10`): este aviso nunca deve cobrir uma confirmação de
	   produção nem um pedido de aprovação esperando resposta. */
	.aviso {
		position: fixed;
		right: 1rem;
		bottom: 1rem;
		z-index: 9;
		width: min(22rem, calc(100vw - 2rem));
		padding: 0.8rem 0.9rem 0.9rem;
		background: var(--panel);
		border: 1px solid var(--rule-strong);
		border-radius: var(--radius);
		box-shadow: 0 8px 24px rgb(10 12 14 / 0.45);
	}

	.titulo {
		margin: 0;
		font-size: 1rem;
		font-weight: 600;
	}

	.motivo {
		margin: 0.2rem 0 0;
		color: var(--text-dim);
		font-size: 0.9rem;
	}

	.notas {
		margin: 0.5rem 0 0;
		padding: 0.45rem 0.6rem;
		max-height: 7rem;
		overflow-y: auto;
		background: var(--panel-dark);
		border-radius: var(--radius);
		font-size: 0.88rem;
		white-space: pre-wrap;
	}

	.tranquilize {
		margin: 0.45rem 0 0;
		color: var(--text-faint);
		font-size: 0.82rem;
	}

	.barra {
		margin-top: 0.6rem;
		height: 4px;
		background: var(--panel-dark);
		border-radius: var(--radius);
		overflow: hidden;
	}

	.cheio {
		height: 100%;
		width: 100%;
		background: var(--action);
		transform-origin: left;
		transition: transform 120ms linear;
	}

	.progresso {
		margin: 0.35rem 0 0;
		color: var(--text-dim);
		font-size: 0.85rem;
		font-family: var(--mono);
	}

	.acoes {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
		margin-top: 0.8rem;
	}
</style>
