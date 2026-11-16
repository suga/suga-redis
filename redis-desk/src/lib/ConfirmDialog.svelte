<script lang="ts">
	import { t } from "$lib/i18n.svelte";
	/**
	 * O último portão antes de uma mudança em produção.
	 *
	 * Só aparece quando o backend recusou a escrita por falta de confirmação
	 * — e o backend só faz isso em host de produção. Por isso o diálogo pode
	 * falar em produção com todas as letras: se ele está na tela, é lá que a
	 * pessoa está mexendo.
	 *
	 * Um clique confirma. Não se digita frase nenhuma: a decisão do produto
	 * é que a pessoa seja avisada, não que seja obrigada a datilografar.
	 */
	interface Props {
		/** A frase vinda de `WriteOutcome::NeedsConfirmation`. */
		summary: string;
		onConfirm: () => void;
		onCancel: () => void;
		busy?: boolean;
	}

	let { summary, onConfirm, onCancel, busy = false }: Props = $props();

	/** O foco começa em Cancelar: a saída segura é a que já está sob o dedo. */
	function focar(el: HTMLElement) {
		el.focus();
	}
</script>

<svelte:window
	onkeydown={(e) => {
		if (e.key === "Escape") onCancel();
	}}
/>

<div
	class="fundo"
	role="presentation"
	onclick={(e) => {
		if (e.target === e.currentTarget) onCancel();
	}}
>
	<div class="caixa" role="dialog" aria-modal="true" aria-labelledby="confirmar-titulo">
		<p class="stamp">{t("ambiente.prod")}</p>
		<h2 id="confirmar-titulo">{t("confirmar.titulo")}</h2>
		<p class="resumo">{summary}</p>

		<div class="acoes">
			<button type="button" onclick={onCancel} use:focar>{t("acao.cancelar")}</button>
			<button type="button" class="primary" onclick={onConfirm} disabled={busy}>
				{busy ? t("acao.aplicando") : t("acao.confirmar")}
			</button>
		</div>
	</div>
</div>

<style>
	.fundo {
		position: fixed;
		inset: 0;
		display: grid;
		place-items: center;
		padding: 1rem;
		background: rgb(10 12 14 / 0.72);
		z-index: 10;
	}

	/* Sem arredondamento, ao contrário do resto do app: a faixa vermelha no
	   topo é a mesma forma da barra do ambiente, e cantos retos fazem as
	   duas serem lidas como a mesma coisa — "você está em produção". */
	.caixa {
		width: min(30rem, 100%);
		padding: 1rem 1.1rem 1.1rem;
		background: var(--panel);
		border: 1px solid var(--rule-strong);
		border-top: 4px solid var(--prod);
		border-radius: 0;
	}

	.stamp {
		margin: 0;
		color: #d4615a;
		font-weight: 700;
		letter-spacing: 0.14em;
		font-size: 0.85rem;
	}

	h2 {
		margin: 0.3rem 0 0.6rem;
		font-size: 1.2rem;
		font-weight: 600;
	}

	.resumo {
		margin: 0;
		padding: 0.55rem 0.7rem;
		background: var(--panel-dark);
		border-radius: var(--radius);
		color: var(--text);
	}

	.acoes {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
		margin-top: 1rem;
	}
</style>
