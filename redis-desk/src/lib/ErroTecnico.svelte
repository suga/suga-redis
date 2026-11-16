<script lang="ts">
	/**
	 * Uma falha, contada em duas camadas.
	 *
	 * Em cima, a frase que diz O QUE falhou, no idioma de quem lê: é com ela
	 * que a pessoa decide se tenta de novo, se liga a VPN ou se chama
	 * alguém. Embaixo, literal e em fonte mono, a mensagem que veio do Rust.
	 *
	 * A mensagem técnica NÃO é traduzida de propósito. Ela é o que se cola
	 * num chamado e o que se procura no código — traduzi-la a esconderia de
	 * quem fosse atrás dela.
	 */
	import { t } from "$lib/i18n.svelte";

	interface Props {
		/** Chave do catálogo que descreve a operação que falhou. */
		assunto: Parameters<typeof t>[0];
		/** A mensagem crua do backend. Vazia quando não há detalhe. */
		detalhe: string;
		/** `data-testid` opcional, para os testes que já procuravam por um. */
		testid?: string;
	}

	let { assunto, detalhe, testid }: Props = $props();
</script>

<div class="erro" role="alert" data-testid={testid}>
	<p class="assunto">{t(assunto)}</p>
	{#if detalhe}
		<p class="detalhe mono" title={t("erro.detalhe")}>{detalhe}</p>
	{/if}
</div>

<style>
	.erro {
		padding: 0.5rem 0.6rem;
		border-radius: 4px;
		background: color-mix(in srgb, var(--danger) 12%, transparent);
		border: 1px solid color-mix(in srgb, var(--danger) 35%, transparent);
	}

	.assunto {
		margin: 0;
		color: var(--danger-text, var(--text));
		font-size: 0.9rem;
	}

	/* Menor e mais apagado que a frase: está ali para ser copiado, não
	   para ser a primeira coisa lida. */
	.detalhe {
		margin: 0.35rem 0 0;
		font-size: 0.78rem;
		color: var(--muted);
		overflow-wrap: anywhere;
	}
</style>
