<script lang="ts">
	import { t } from "$lib/i18n.svelte";
	/**
	 * A busca por texto dentro dos valores.
	 *
	 * O que ela responde vem do ÍNDICE, que é um retrato de um instante
	 * passado. Por isso a tela nunca mostra só a lista: mostra também de
	 * quando é o retrato e quantas chaves já não existiam mais. Uma busca
	 * que esconde a própria idade convida a pessoa a confiar num dado velho.
	 */
	import { haQuantoTempo, numero, partirNoAcerto, formatarPrazo } from "$lib/formato";
	import {
		NEVER_SCANNED,
		errorMessage,
		rescan,
		search,
		type KeyKind,
		type SearchOutcome,
	} from "$lib/api";

	interface Props {
		hostId: string;
		db: number;
		/** Abre a chave do acerto na árvore. */
		onOpenKey: (key: string) => void;
	}

	let { hostId, db, onOpenKey }: Props = $props();

	/**
	 * A partir de quanto tempo o índice merece um aviso. Uma hora: tempo
	 * suficiente para um banco movimentado ter mudado bastante, curto o
	 * bastante para não avisar a cada busca de quem acabou de varrer.
	 */
	const INDICE_VELHO_SEGUNDOS = 3600;

	let agulha = $state("");
	let resultado = $state<SearchOutcome | null>(null);
	let buscando = $state(false);
	let varrendo = $state(false);
	let erro = $state("");
	/** A agulha da última busca concluída — não a que está sendo digitada. */
	let buscada = $state("");

	async function buscar() {
		const texto = agulha.trim();
		if (!texto) return;
		buscando = true;
		erro = "";
		try {
			resultado = await search(hostId, db, texto);
			buscada = texto;
		} catch (err) {
			resultado = null;
			erro = errorMessage(err);
		} finally {
			buscando = false;
		}
	}

	async function varrerEBuscar() {
		varrendo = true;
		erro = "";
		try {
			await rescan(hostId, db);
			await buscar();
		} catch (err) {
			erro = errorMessage(err);
		} finally {
			varrendo = false;
		}
	}

	function nomeDoTipo(kind: KeyKind): string {
		return typeof kind === "string" ? kind : kind.unknown;
	}

	let indiceVelho = $derived(
		resultado !== null &&
			(resultado.index_age_secs === NEVER_SCANNED ||
				resultado.index_age_secs > INDICE_VELHO_SEGUNDOS),
	);
</script>

<section>
	<form
		class="campo"
		onsubmit={(e) => {
			e.preventDefault();
			buscar();
		}}
	>
		<label class="grow">
			<span class="rotulo">{t("busca.titulo")}</span>
			<input
				bind:value={agulha}
				class="mono"
				autocomplete="off"
				placeholder={t("busca.placeholder")}
			/>
		</label>
		<button type="submit" class="primary" disabled={buscando || !agulha.trim()}>
			{buscando ? t("acao.buscando") : t("acao.buscar")}
		</button>
	</form>

	{#if erro}
		<p class="erro" role="alert">{erro}</p>
	{:else if resultado}
		{#if indiceVelho}
			<div class="velho" data-testid="indice-velho">
				<span>
					{resultado.index_age_secs === NEVER_SCANNED
						? t("busca.nuncaVarrido")
						: t("busca.indiceVelho", {
								quando: haQuantoTempo(resultado.index_age_secs),
							})}
				</span>
				<button onclick={varrerEBuscar} disabled={varrendo}>
					{varrendo ? t("varredura.semTotal") : t("acao.varrerDeNovo")}
				</button>
			</div>
		{/if}

		{#if resultado.hits.length === 0}
			<p class="sem-acerto" data-testid="sem-acerto">
				{t("busca.semAcerto")}
			</p>
		{:else}
			<ul>
				{#each resultado.hits as hit (hit.key)}
					{@const partes = partirNoAcerto(hit.snippet, hit.match_start, hit.match_len)}
					<li data-testid="acerto">
						<button class="acerto" onclick={() => onOpenKey(hit.key)}>
							<span class="chave mono">{hit.key}</span>
							<span class="meta">
								<span class="tipo mono">{nomeDoTipo(hit.kind)}</span>
								{#if hit.ttl_ms !== null}
									<span class="prazo mono">{t("busca.expiraEm", { prazo: formatarPrazo(hit.ttl_ms) })}</span>
								{/if}
								<span class="onde">
									{hit.where_found === "key_name" ? t("busca.achadoNoNome") : t("busca.achadoNoValor")}
								</span>
							</span>
							<span class="trecho mono" data-testid="trecho"
								>{partes.antes}<mark>{partes.acerto}</mark>{partes.depois}</span
							>
						</button>
					</li>
				{/each}
			</ul>
		{/if}

		<p class="aviso" data-testid="aviso">
			{t("busca.conferiu", {
				chaves: numero(resultado.scanned),
				ms: resultado.elapsed_ms,
			})}
			{#if resultado.index_age_secs !== NEVER_SCANNED}
				{t("busca.indiceVarrido", { quando: haQuantoTempo(resultado.index_age_secs) })}
			{/if}
			{#if resultado.stale_keys_removed > 0}
				{resultado.stale_keys_removed === 1
					? t("busca.sumidaUma", { quantas: numero(resultado.stale_keys_removed) })
					: t("busca.sumidasVarias", {
							quantas: numero(resultado.stale_keys_removed),
						})}
			{/if}
		</p>
	{:else}
		<p class="convite">
			{t("busca.convite")}
		</p>
	{/if}
</section>

<style>
	section {
		display: flex;
		flex-direction: column;
		gap: 0.8rem;
		min-width: 0;
	}

	.campo {
		display: flex;
		gap: 0.5rem;
		align-items: flex-end;
	}

	.grow {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.rotulo {
		color: var(--text-dim);
		font-size: 0.9rem;
	}

	.velho {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
		padding: 0.45rem 0.65rem;
		background: var(--panel-dark);
		border: 1px solid var(--rule-strong);
		border-radius: var(--radius);
		color: var(--text-dim);
		font-size: 0.92rem;
	}

	.velho button {
		white-space: nowrap;
	}

	ul {
		list-style: none;
		margin: 0;
		padding: 0;
	}

	li {
		border-bottom: 1px solid var(--rule);
	}

	.acerto {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		width: 100%;
		padding: 0.5rem 0.4rem;
		border: none;
		border-radius: 0;
		text-align: left;
	}

	.acerto:hover {
		background: var(--panel-dark);
	}

	.chave {
		color: var(--text);
		word-break: break-all;
	}

	.meta {
		display: flex;
		flex-wrap: wrap;
		gap: 0.9rem;
		color: var(--text-faint);
		font-size: 0.88rem;
	}

	.trecho {
		color: var(--text-dim);
		word-break: break-word;
	}

	mark {
		background: rgb(242 163 60 / 0.26);
		color: var(--text);
		border-radius: 1px;
	}

	.aviso,
	.convite,
	.sem-acerto {
		margin: 0;
		color: var(--text-faint);
		font-size: 0.92rem;
	}

	.sem-acerto {
		padding: 0.8rem 0.4rem;
		color: var(--text-dim);
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
