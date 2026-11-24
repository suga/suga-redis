<script lang="ts">
	/**
	 * O que fica entre uma pessoa e apagar tudo que há sob um prefixo.
	 *
	 * A trava não é deste arquivo: quem recusa a escrita em produção é
	 * `commands.rs`, onde um botão não pode esquecer de perguntar. O que é
	 * deste arquivo é a outra metade do consentimento informado — é aqui que
	 * a pessoa aprende o que está prestes a fazer. Um diálogo que engana não
	 * falha alto: devolve uma decisão errada com toda a confiança.
	 *
	 * As quatro coisas que ele precisa dizer, e por quê:
	 *
	 * - **A contagem da enumeração**, não a da árvore. A da árvore é um
	 *   número guardado de uma varredura antiga; a daqui foi conferida agora.
	 *   As duas diferem, e essa diferença é a razão de a enumeração ser um
	 *   passo à parte.
	 * - **A cópia vem marcada.** O caminho com volta atrás é o padrão;
	 *   abrir mão dela é um ato, não um esquecimento.
	 * - **Quantas chaves não voltam nem com a cópia.** Dito com a cópia
	 *   ainda marcada, que é justamente o caso em que alguém se acha coberto
	 *   e não está: a cópia guarda set, stream e RedisJSON, mas a volta
	 *   atrás não os recria.
	 * - **Que a varredura parou no teto**, quando parou: são mais de 100.000
	 *   chaves, e uma cópia desse tamanho não cabe no histórico.
	 *
	 * Ele não fala com o Rust: recebe tudo por prop e devolve a decisão por
	 * `onConfirmar`. Quem varre, apaga e cancela é quem o abriu.
	 */
	import { envChrome } from "$lib/env";
	import { numero } from "$lib/formato";
	import { t } from "$lib/i18n.svelte";
	import type { BackupRequest, EnumerationHandle, Host } from "$lib/api";

	interface Props {
		host: Host;
		db: number;
		prefixos: string[];
		/** `null` enquanto a varredura ainda corre. */
		enumeracao: EnumerationHandle | null;
		progresso: { scanned: number; total: number } | null;
		/**
		 * Não-nulo enquanto as chaves saem: quantas já saíram.
		 *
		 * É ele que trava o botão contra o segundo clique, então quem chama
		 * precisa marcá-lo ANTES de pedir a exclusão — e não depois do
		 * primeiro andamento chegar.
		 */
		apagados: number | null;
		onConfirmar: (backup: BackupRequest, nomeDigitado: string | null) => void;
		onCancelar: () => void;
	}

	let {
		host,
		db,
		prefixos,
		enumeracao,
		progresso,
		apagados,
		onConfirmar,
		onCancelar,
	}: Props = $props();

	/** Quantos prefixos a lista mostra antes de resumir o resto num número. */
	const MOSTRADOS = 5;

	let comCopia = $state(true);
	let digitado = $state("");

	let chrome = $derived(envChrome(host.env));
	let ehProd = $derived(host.env === "prod");
	let varrendo = $derived(enumeracao === null);
	let apagando = $derived(apagados !== null);

	/**
	 * Acima do teto a cópia só pode ir para arquivo: 100.000 retratos dentro
	 * de uma linha do histórico não é cópia, é outro banco.
	 */
	let soEmArquivo = $derived(enumeracao?.truncated ?? false);

	let backup = $derived<BackupRequest>(
		!comCopia ? "none" : soEmArquivo ? "file" : "inline",
	);

	/**
	 * Byte a byte, como o Rust compara: sem aparar espaço e sem ignorar
	 * caixa. Aparar seria decidir pela pessoa que ela quis dizer outra coisa.
	 */
	let confere = $derived(digitado === host.name);
	let liberado = $derived(!apagando && (!ehProd || confere));

	let mostrados = $derived(prefixos.slice(0, MOSTRADOS));
	let restantes = $derived(prefixos.length - mostrados.length);

	let textoContagem = $derived.by(() => {
		const n = enumeracao?.count ?? 0;
		const chave = n === 1 ? "apagarPrefixo.contagemUma" : "apagarPrefixo.contagemVarias";
		return t(chave, { n: numero(n) });
	});

	let textoSemVolta = $derived.by(() => {
		const n = enumeracao?.unrestorable ?? 0;
		const chave = n === 1 ? "apagarPrefixo.semVoltaUma" : "apagarPrefixo.semVoltaVarias";
		return t(chave, { n: numero(n) });
	});

	/** O botão carrega a contagem e diz se há volta atrás por trás dele. */
	let textoApagar = $derived.by(() => {
		const n = enumeracao?.count ?? 0;
		const uma = n === 1;
		const chave = comCopia
			? uma
				? "apagarPrefixo.apagarUma"
				: "apagarPrefixo.apagarVarias"
			: uma
				? "apagarPrefixo.apagarUmaSemVolta"
				: "apagarPrefixo.apagarVariasSemVolta";
		return t(chave, { n: numero(n) });
	});

	let textoApagando = $derived.by(() => {
		const n = apagados ?? 0;
		const chave = n === 1 ? "apagarPrefixo.apagandoUma" : "apagarPrefixo.apagandoVarias";
		return t(chave, { n: numero(n) });
	});

	function confirmar() {
		if (!liberado) return;
		onConfirmar(backup, ehProd ? digitado : null);
	}

	/** O foco começa em Cancelar: a saída segura é a que já está sob o dedo. */
	function focar(el: HTMLElement) {
		el.focus();
	}
</script>

<svelte:window
	onkeydown={(e) => {
		if (e.key === "Escape") onCancelar();
	}}
/>

<div
	class="fundo"
	role="presentation"
	onclick={(e) => {
		if (e.target === e.currentTarget) onCancelar();
	}}
>
	<div
		class="caixa"
		role="dialog"
		aria-modal="true"
		aria-labelledby="apagar-titulo"
		style="--env-band: {chrome.band}"
	>
		<p class="stamp">{chrome.label}</p>
		<h2 id="apagar-titulo">{t("apagarPrefixo.titulo")}</h2>

		<dl>
			<div>
				<dt>{t("campo.host")}</dt>
				<dd class="mono">{host.name}</dd>
			</div>
			<div>
				<dt>{t("campo.banco")}</dt>
				<dd class="mono">{db}</dd>
			</div>
		</dl>

		<p class="rotulo">{t("apagarPrefixo.prefixos")}</p>
		<ul class="prefixos">
			{#each mostrados as prefixo}
				<li class="mono">{prefixo}</li>
			{/each}
			{#if restantes > 0}
				<li class="resto">{t("apagarPrefixo.eMais", { n: numero(restantes) })}</li>
			{/if}
		</ul>

		{#if enumeracao}
			<p class="contagem">{textoContagem}</p>

			{#if enumeracao.truncated}
				<p class="aviso">{t("apagarPrefixo.acimaDoTeto")}</p>
			{/if}

			{#if enumeracao.unrestorable > 0}
				<p class="aviso">{textoSemVolta}</p>
			{/if}

			<label class="copia">
				<input type="checkbox" bind:checked={comCopia} disabled={apagando} />
				<span>
					{soEmArquivo
						? t("apagarPrefixo.copiaArquivo")
						: t("apagarPrefixo.copiaHistorico")}
				</span>
			</label>

			{#if !comCopia}
				<p class="irreversivel" role="status">{t("apagarPrefixo.irreversivel")}</p>
			{/if}

			{#if ehProd}
				<label class="nome">
					<span>{t("apagarPrefixo.digiteNome")}</span>
					<input bind:value={digitado} class="mono" autocomplete="off" disabled={apagando} />
				</label>
				{#if digitado && !confere}
					<p class="dica">
						{t("apagarPrefixo.digiteExatamente")}
						<span class="mono">{host.name}</span>
					</p>
				{/if}
			{/if}
		{:else}
			<p class="andamento" role="status">
				{t("apagarPrefixo.varrendo", {
					feitas: numero(progresso?.scanned ?? 0),
					total: numero(progresso?.total ?? 0),
				})}
			</p>
		{/if}

		{#if apagando}
			<p class="andamento" role="status">{textoApagando}</p>
		{/if}

		<div class="acoes">
			<button type="button" onclick={onCancelar} use:focar>{t("acao.cancelar")}</button>
			{#if !varrendo}
				<button type="button" class="primary" onclick={confirmar} disabled={!liberado}>
					{textoApagar}
				</button>
			{/if}
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

	dt,
	.rotulo {
		color: var(--text-faint);
		font-size: 0.9rem;
	}

	dd {
		margin: 0;
		color: var(--text);
		word-break: break-all;
	}

	.rotulo {
		margin: 0 0 0.2rem;
	}

	.prefixos {
		margin: 0;
		padding: 0.45rem 0.7rem;
		list-style: none;
		background: var(--panel-dark);
		border-radius: var(--radius);
	}

	.prefixos li {
		word-break: break-all;
	}

	.resto {
		color: var(--text-faint);
		font-size: 0.9rem;
	}

	/* A contagem é o número que a decisão pesa: fica maior que a prosa em
	   volta, sem cor nenhuma — o vermelho `--prod` significa só o ambiente,
	   e emprestá-lo para marcar perigo deixaria o carimbo ambíguo. */
	.contagem {
		margin: 0.7rem 0 0;
		font-size: 1.05rem;
		font-weight: 600;
	}

	.aviso,
	.irreversivel,
	.dica,
	.andamento {
		margin: 0.4rem 0 0;
		color: var(--text-dim);
		font-size: 0.9rem;
	}

	/* O irreversível se distingue por peso e por um traço ao lado, não por
	   cor, pela mesma razão. */
	.irreversivel {
		padding-left: 0.6rem;
		border-left: 2px solid var(--text-dim);
		color: var(--text);
		font-weight: 600;
	}

	.copia {
		display: flex;
		gap: 0.45rem;
		align-items: baseline;
		margin-top: 0.8rem;
	}

	.nome {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		margin-top: 0.8rem;
	}

	.nome > span {
		color: var(--text-dim);
		font-size: 0.9rem;
	}

	.acoes {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
		margin-top: 1rem;
	}
</style>
