<script lang="ts">
	import ErroTecnico from "$lib/ErroTecnico.svelte";
	import { t } from "$lib/i18n.svelte";
	/**
	 * Criar chave: nome e tipo.
	 *
	 * Só os quatro tipos editáveis aparecem. `set`, `stream` e RedisJSON são
	 * somente leitura neste produto, então oferecê-los aqui criaria uma
	 * chave que a própria tela não saberia editar depois.
	 */
	interface NovaChave {
		key: string;
		kind: "string" | "hash" | "list" | "zset";
	}

	interface Props {
		onCreate: (nova: NovaChave) => void;
		onCancel: () => void;
		busy?: boolean;
	}

	let { onCreate, onCancel, busy = false }: Props = $props();

	const TIPOS = [
		{ value: "string", chave: "tipo.string" },
		{ value: "hash", chave: "tipo.hash" },
		{ value: "list", chave: "tipo.list" },
		{ value: "zset", chave: "tipo.zset" },
	] as const satisfies { value: NovaChave["kind"]; chave: Parameters<typeof t>[0] }[];

	let nome = $state("");
	let tipo = $state<NovaChave["kind"]>("string");
	let erro = $state("");

	function criar(event: Event) {
		event.preventDefault();
		const chave = nome.trim();
		if (!chave) {
			erro = "Informe o nome da chave.";
			return;
		}
		erro = "";
		onCreate({ key: chave, kind: tipo });
	}

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
	<div class="caixa" role="dialog" aria-modal="true" aria-labelledby="nova-titulo">
		<h2 id="nova-titulo">{t("novaChave.titulo")}</h2>

		<form onsubmit={criar}>

			<label>
				<span>{t("novaChave.nome")}</span>
				<input bind:value={nome} class="mono" autocomplete="off" use:focar />
			</label>

			<label>
				<span>{t("campo.tipo")}</span>
				<select bind:value={tipo}>
					{#each TIPOS as opcao (opcao.value)}
						<option value={opcao.value}>{t(opcao.chave)}</option>
					{/each}
				</select>
			</label>

			{#if erro}
				<ErroTecnico assunto="erro.aoCriarChave" detalhe={erro} />
			{/if}

			<div class="acoes">
				<button type="button" onclick={onCancel}>{t("acao.cancelar")}</button>
				<button type="submit" class="primary" disabled={busy}>{t("acao.criar")}</button>
			</div>
		</form>
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

	.caixa {
		width: min(28rem, 100%);
		padding: 1rem 1.1rem 1.1rem;
		background: var(--panel);
		border: 1px solid var(--rule-strong);
		border-radius: var(--radius);
	}

	form {
		display: flex;
		flex-direction: column;
		gap: 0.7rem;
	}

	h2 {
		margin: 0 0 0.7rem;
		font-size: 1.2rem;
		font-weight: 600;
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

	.acoes {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
		margin-top: 0.3rem;
	}

</style>
