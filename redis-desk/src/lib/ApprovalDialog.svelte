<script lang="ts">
	import { t } from "$lib/i18n.svelte";
	import { resumoDaOperacao } from "$lib/resumo";
	/**
	 * O pedido de escrita que o Claude mandou, esperando a decisão do dono.
	 *
	 * Enquanto este diálogo está na tela, NADA foi escrito: o processo do
	 * servidor MCP está parado esperando a resposta daqui. Fechar a janela,
	 * recusar ou simplesmente não responder dão todos no mesmo resultado
	 * para o banco — nada muda.
	 *
	 * Para liberar, o dono digita o nome do host. Não é burocracia: é o que
	 * separa "eu li o que vai acontecer e em qual banco" de "eu cliquei no
	 * que apareceu na minha frente". É a única tela do app que pede isso, e
	 * pede justamente porque quem pediu a mudança não foi uma pessoa.
	 */
	import { envChrome } from "$lib/env";
	import {
		denyApproval,
		errorMessage,
		resolveApproval,
		type ApprovalRequest,
	} from "$lib/api";

	interface Props {
		request: ApprovalRequest;
		/** Chamado quando o pedido deixa de estar pendente. */
		onSettled: () => void;
	}

	let { request, onSettled }: Props = $props();

	let digitado = $state("");
	let ocupado = $state(false);
	let erro = $state("");

	let chrome = $derived(envChrome(request.env));
	let confere = $derived(digitado === request.host_name);

	async function liberar() {
		if (!confere) return;
		ocupado = true;
		erro = "";
		try {
			await resolveApproval(request.id, digitado);
			onSettled();
		} catch (err) {
			erro = errorMessage(err);
		} finally {
			ocupado = false;
		}
	}

	async function recusar() {
		ocupado = true;
		erro = "";
		try {
			await denyApproval(request.id);
			onSettled();
		} catch (err) {
			erro = errorMessage(err);
		} finally {
			ocupado = false;
		}
	}

	function focar(el: HTMLElement) {
		el.focus();
	}
</script>

<div class="fundo" role="presentation">
	<div
		class="caixa"
		role="dialog"
		aria-modal="true"
		aria-labelledby="pedido-titulo"
		style="--env-band: {chrome.band}; --env-border: {chrome.border}"
	>
		<p class="stamp">{chrome.label}</p>
		<h2 id="pedido-titulo">{t("aprovacao.titulo")}</h2>

		<dl>
			<div>
				<dt>{t("campo.host")}</dt>
				<dd class="mono">{request.host_name}</dd>
			</div>
			<div>
				<dt>{t("campo.banco")}</dt>
				<dd class="mono">{request.db}</dd>
			</div>
			<div>
				<dt>{t("campo.chave")}</dt>
				<dd class="mono">{request.key}</dd>
			</div>
		</dl>

		<p class="resumo" data-testid="resumo-pedido">
			{resumoDaOperacao(request.key, request.op)}
		</p>

		<label>
			<span>{t("aprovacao.digiteNome")}</span>
			<input bind:value={digitado} class="mono" autocomplete="off" use:focar />
		</label>

		{#if digitado && !confere}
			<p class="dica" role="status">
				{t("aprovacao.digiteExatamente")} <span class="mono">{request.host_name}</span>
			</p>
		{/if}

		{#if erro}
			<p class="erro" role="alert">{erro}</p>
		{/if}

		<div class="acoes">
			<button type="button" onclick={recusar} disabled={ocupado}>{t("aprovacao.recusar")}</button>
			<button
				type="button"
				class="primary"
				onclick={liberar}
				disabled={!confere || ocupado}
			>
				{t("aprovacao.liberar")}
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
		background: rgb(10 12 14 / 0.78);
		z-index: 20;
	}

	/* Cantos retos e faixa colorida no topo: a mesma forma da barra do
	   ambiente, para o diálogo ser lido como parte do mesmo aviso. */
	.caixa {
		width: min(34rem, 100%);
		padding: 1rem 1.1rem 1.1rem;
		background: var(--panel);
		border: 1px solid var(--rule-strong);
		border-top: 4px solid var(--env-band);
		border-radius: 0;
	}

	.stamp {
		margin: 0;
		color: var(--text-dim);
		font-weight: 700;
		letter-spacing: 0.14em;
		font-size: 0.85rem;
	}

	h2 {
		margin: 0.3rem 0 0.7rem;
		font-size: 1.2rem;
		font-weight: 600;
	}

	dl {
		display: flex;
		flex-wrap: wrap;
		gap: 0.3rem 1.6rem;
		margin: 0 0 0.7rem;
	}

	dl > div {
		display: flex;
		gap: 0.4rem;
		align-items: baseline;
		min-width: 0;
	}

	dt {
		color: var(--text-faint);
		font-size: 0.9rem;
	}

	dd {
		margin: 0;
		color: var(--text);
		word-break: break-all;
	}

	.resumo {
		margin: 0 0 0.8rem;
		padding: 0.55rem 0.7rem;
		background: var(--panel-dark);
		border-radius: var(--radius);
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

	.dica {
		margin: 0.4rem 0 0;
		color: var(--text-faint);
		font-size: 0.9rem;
	}

	.erro {
		margin: 0.5rem 0 0;
		padding: 0.4rem 0.6rem;
		border: 1px solid #5e3a38;
		border-radius: var(--radius);
		background: #2e2326;
		color: #e49a94;
	}

	.acoes {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
		margin-top: 1rem;
	}
</style>
