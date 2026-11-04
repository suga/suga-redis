<script lang="ts">
	/**
	 * O painel de uma chave: cabeçalho com tipo, tamanho e prazo, e o
	 * conteúdo abaixo.
	 *
	 * É quem decide se a chave pode ser editada. Tipo somente leitura
	 * (`set`, `stream`, RedisJSON) nunca recebe editor, mesmo que quem use
	 * este componente passe um — a regra é do produto, e deixá-la na mão de
	 * cada tela daria a cada tela uma chance de errá-la.
	 */
	import type { Snippet } from "svelte";
	import ReadOnlyView from "$lib/editors/ReadOnlyView.svelte";
	import { formatarPrazo, formatarTamanho, numero } from "$lib/formato";
	import {
		errorMessage,
		isEditable,
		readValue,
		readValueMore,
		type Bytes,
		type Value,
		type ValueView,
	} from "$lib/api";

	interface Props {
		hostId: string;
		db: number;
		keyName: string;
		/** O editor do tipo, quando existe um. Ignorado em tipo só leitura. */
		editor?: Snippet<[ValueView]>;
		/** Avisa a tela que esta chave não existe mais (Review Focus 3). */
		onKeyGone?: (key: string) => void;
	}

	let { hostId, db, keyName, editor, onKeyGone }: Props = $props();

	let view = $state<ValueView | null>(null);
	let carregando = $state(false);
	let carregandoMais = $state(false);
	let erro = $state("");

	/**
	 * Quanto do valor já está na tela, na unidade em que o backend conta.
	 *
	 * Para uma coleção são elementos. Para uma string são BYTES — é o que o
	 * `total_elements` dela traz (o `STRLEN`), e é o que o backend espera de
	 * volta como `offset` ao pedir a página seguinte. Contar "1" aqui, como
	 * se a string fosse um elemento só, fazia o pedido seguinte voltar para
	 * o byte 1 e reler sempre o mesmo começo: um valor de 112 kB nunca
	 * passava dos primeiros 64 kB.
	 */
	function contar(value: Value): number {
		switch (value.kind) {
			case "string":
				return bytesDe(value.data.text ?? "");
			case "read_only_text":
				return bytesDe(value.data);
			default:
				return value.data.length;
		}
	}

	/** Tamanho em bytes de UTF-8 — não em unidades UTF-16, como `length`. */
	function bytesDe(texto: string): number {
		return new TextEncoder().encode(texto).length;
	}

	/** Emenda a fatia nova no fim da que já está na tela, do mesmo tipo. */
	function emendar(atual: Value, novo: Value): Value {
		if (atual.kind !== novo.kind) return novo;
		switch (atual.kind) {
			// Uma string grande chega em pedaços e precisa ser COLADA. Trocar
			// pelo pedaço novo deixaria na tela só o fim do valor — e salvar
			// a partir dali apagaria tudo que veio antes.
			case "string":
				return {
					kind: "string",
					data: {
						...(novo.data as Bytes),
						text: (atual.data.text ?? "") + ((novo.data as Bytes).text ?? ""),
					},
				};
			case "read_only_text":
				return { kind: "read_only_text", data: atual.data + (novo.data as string) };
			default:
				return {
					kind: atual.kind,
					data: [...atual.data, ...(novo.data as never[])],
				} as Value;
		}
	}

	async function carregar(id: string, banco: number, chave: string) {
		carregando = true;
		erro = "";
		try {
			view = await readValue(id, banco, chave);
		} catch (err) {
			view = null;
			erro = errorMessage(err);
			if (
				typeof err === "object" &&
				err !== null &&
				"kind" in err &&
				err.kind === "key_gone"
			) {
				onKeyGone?.(chave);
			}
		} finally {
			carregando = false;
		}
	}

	$effect(() => {
		carregar(hostId, db, keyName);
	});

	async function carregarMais() {
		if (!view) return;
		carregandoMais = true;
		try {
			const proxima = await readValueMore(hostId, db, keyName, contar(view.value));
			view = {
				...proxima,
				value: emendar(view.value, proxima.value),
			};
		} catch (err) {
			erro = errorMessage(err);
		} finally {
			carregandoMais = false;
		}
	}

	// ----------------------------------------------------------- prazo

	let restanteMs = $state<number | null>(null);

	/**
	 * A contagem anda sozinha. Conta pelo relógio (`Date.now`) em vez de
	 * subtrair mil a cada disparo: um `setInterval` atrasa quando a aba fica
	 * em segundo plano, e o prazo mostrado passaria a mentir para mais.
	 */
	$effect(() => {
		const total = view?.ttl_ms ?? null;
		if (total === null) {
			restanteMs = null;
			return;
		}

		const comeco = Date.now();
		restanteMs = total;
		const timer = setInterval(() => {
			restanteMs = total - (Date.now() - comeco);
		}, 1000);
		return () => clearInterval(timer);
	});

	function nomeDoTipo(kind: ValueView["kind"]): string {
		return typeof kind === "string" ? kind : kind.unknown;
	}

	let editavel = $derived(view ? isEditable(view.kind) : false);
	let mostrados = $derived(view ? contar(view.value) : 0);
</script>

<section>
	{#if erro}
		<p class="erro" role="alert">{erro}</p>
	{:else if carregando && !view}
		<p class="aviso">Carregando a chave…</p>
	{:else if view}
		<header>
			<h2 class="mono">{view.key}</h2>
			<dl>
				<div>
					<dt>Tipo</dt>
					<dd class="mono" data-testid="kind">{nomeDoTipo(view.kind)}</dd>
				</div>
				<div>
					<dt>Tamanho</dt>
					<dd class="mono">{formatarTamanho(view.size_bytes)}</dd>
				</div>
				{#if restanteMs !== null}
					<div>
						<dt>Expira em</dt>
						<dd class="mono" data-testid="ttl">{formatarPrazo(restanteMs)}</dd>
					</div>
				{/if}
				{#if !editavel}
					<div>
						<dt>Edição</dt>
						<dd>Somente leitura</dd>
					</div>
				{/if}
			</dl>
		</header>

		{#if editavel && editor}
			{@render editor(view)}
		{:else}
			<ReadOnlyView value={view.value} />
		{/if}

		{#if view.truncated}
			<footer data-testid="truncado">
				<span>
					Mostrando {numero(mostrados)} de
					{numero(view.total_elements)}
				</span>
				<button onclick={carregarMais} disabled={carregandoMais}>
					{carregandoMais ? "Carregando" : "Carregar mais"}
				</button>
			</footer>
		{/if}
	{/if}
</section>

<style>
	section {
		display: flex;
		flex-direction: column;
		gap: 0.8rem;
		min-width: 0;
	}

	header {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		padding-bottom: 0.6rem;
		border-bottom: 1px solid var(--rule);
	}

	h2 {
		margin: 0;
		font-size: 1.05rem;
		font-weight: 500;
		word-break: break-all;
	}

	dl {
		display: flex;
		flex-wrap: wrap;
		gap: 0.25rem 1.6rem;
		margin: 0;
	}

	dl > div {
		display: flex;
		gap: 0.4rem;
		align-items: baseline;
	}

	dt {
		color: var(--text-faint);
		font-size: 0.9rem;
	}

	dd {
		margin: 0;
		color: var(--text-dim);
	}

	footer {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
		padding-top: 0.5rem;
		border-top: 1px solid var(--rule);
		color: var(--text-dim);
	}

	.aviso {
		margin: 0;
		color: var(--text-faint);
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
