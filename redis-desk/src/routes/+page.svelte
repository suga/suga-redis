<script lang="ts">
	/**
	 * A casca: barra do ambiente em cima, coluna de hosts à esquerda, área de
	 * trabalho à direita. A área de trabalho ganha a árvore de chaves e o
	 * painel de valor na Task 11.
	 */
	import { onMount } from "svelte";
	import ApprovalDialog from "$lib/ApprovalDialog.svelte";
	import EnvBand from "$lib/EnvBand.svelte";
	import HostForm from "$lib/HostForm.svelte";
	import HostList from "$lib/HostList.svelte";
	import Workspace from "$lib/Workspace.svelte";
	import {
		errorMessage,
		listHosts,
		onApprovalsChanged,
		pendingApprovals,
		type ApprovalRequest,
		type Host,
	} from "$lib/api";

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

	// ------------------------------- escritas pedidas pelo Claude

	let pedidos = $state<ApprovalRequest[]>([]);

	async function lerPedidos() {
		try {
			pedidos = await pendingApprovals();
		} catch {
			// A fila não responder não pode apagar a tela: o pior caso é o
			// pedido aparecer um instante depois, na próxima rodada.
		}
	}

	onMount(() => {
		lerPedidos();
		const parar = onApprovalsChanged(lerPedidos);
		// O evento é o caminho rápido; a varredura periódica é a garantia de
		// que um evento perdido não deixa um pedido invisível até expirar.
		const relogio = setInterval(lerPedidos, 3000);
		return () => {
			clearInterval(relogio);
			parar.then((fn) => fn());
		};
	});

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
				{#key aberto.id}
					<Workspace host={aberto} />
				{/key}
			{:else}
				<p class="placeholder">Escolha um host na coluna ao lado.</p>
			{/if}
		</main>
	</div>
</div>

<!-- Um pedido por vez: decidir dois ao mesmo tempo é decidir nenhum com
     atenção, e o resto da fila continua esperando sem risco. -->
{#if pedidos.length > 0}
	<ApprovalDialog request={pedidos[0]} onSettled={lerPedidos} />
{/if}

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
		min-width: 0;
		min-height: 0;
		overflow: hidden;
		display: flex;
		flex-direction: column;
	}

	/* A casca não põe preenchimento em volta da área de trabalho: a árvore
	   encosta na borda de propósito, para caber o máximo de chaves. */
	main > :global(.placeholder),
	main > :global(form),
	main > :global(.erro) {
		margin: 1.2rem;
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
