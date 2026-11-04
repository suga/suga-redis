<script lang="ts">
	/**
	 * A casca: barra do ambiente em cima, coluna de hosts à esquerda, área de
	 * trabalho à direita. A área de trabalho ganha a árvore de chaves e o
	 * painel de valor na Task 11.
	 */
	import { onMount } from "svelte";
	import EnvBand from "$lib/EnvBand.svelte";
	import HostForm from "$lib/HostForm.svelte";
	import HostList from "$lib/HostList.svelte";
	import { errorMessage, listHosts, type Host } from "$lib/api";

	let hosts = $state<Host[]>([]);
	let aberto = $state<Host | null>(null);
	let erro = $state("");
	/** `null` = nenhum formulário; `{ host: null }` = cadastro novo. */
	let editando = $state<{ host: Host | null } | null>(null);

	async function carregar() {
		erro = "";
		try {
			hosts = await listHosts();
			// O host aberto pode ter sido editado ou removido: refaz a
			// referência a partir da lista nova em vez de manter a antiga.
			aberto = aberto ? (hosts.find((h) => h.id === aberto?.id) ?? null) : null;
		} catch (err) {
			erro = errorMessage(err);
		}
	}

	// `onMount` e não `$effect`: `carregar` lê e escreve `aberto`, e um
	// efeito que depende do que ele mesmo escreve se chamaria em laço.
	onMount(carregar);

	function salvou(hostId: string) {
		editando = null;
		carregar().then(() => {
			aberto = hosts.find((h) => h.id === hostId) ?? aberto;
		});
	}
</script>

<div class="app">
	<EnvBand host={aberto} />

	<div class="corpo">
		<aside>
			<HostList
				{hosts}
				selectedId={aberto?.id ?? null}
				onSelect={(host) => {
					aberto = host;
					editando = null;
				}}
				onNew={() => (editando = { host: null })}
				onEdit={(host) => (editando = { host })}
				onChanged={carregar}
			/>
		</aside>

		<main>
			{#if erro}
				<p class="erro" role="alert">{erro}</p>
			{/if}

			{#if editando}
				{#key editando.host?.id ?? "novo"}
					<HostForm
						host={editando.host}
						onSaved={salvou}
						onCancel={() => (editando = null)}
					/>
				{/key}
			{:else if aberto}
				<p class="placeholder">
					A árvore de chaves de <span class="mono">{aberto.name}</span> aparece aqui.
				</p>
			{:else}
				<p class="placeholder">Escolha um host na coluna ao lado.</p>
			{/if}
		</main>
	</div>
</div>

<style>
	.app {
		display: flex;
		flex-direction: column;
		height: 100vh;
	}

	.corpo {
		display: grid;
		grid-template-columns: minmax(14rem, 20rem) 1fr;
		flex: 1;
		min-height: 0;
	}

	aside {
		min-height: 0;
		overflow: hidden;
	}

	main {
		padding: 1.2rem;
		overflow-y: auto;
	}

	.placeholder {
		margin: 0;
		color: var(--text-faint);
	}

	.erro {
		margin: 0 0 1rem;
		padding: 0.4rem 0.6rem;
		border: 1px solid #5e3a38;
		border-radius: var(--radius);
		background: #2e2326;
		color: #e49a94;
	}

	@media (max-width: 40rem) {
		.corpo {
			grid-template-columns: 1fr;
			grid-template-rows: auto 1fr;
		}
	}
</style>
