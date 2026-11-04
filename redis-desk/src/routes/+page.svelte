<script lang="ts">
	// Tela provisória: só confirma que a fachada de comandos responde.
	// A Task 10 troca isto pela casca de verdade (barra do ambiente, lista
	// de hosts, cadastro).
	import { errorMessage, listHosts, type Host } from "$lib/api";

	let hosts = $state<Host[] | null>(null);
	let erro = $state("");

	async function carregar() {
		erro = "";
		try {
			hosts = await listHosts();
		} catch (err) {
			erro = errorMessage(err);
		}
	}
</script>

<main>
	<h1>redis-desk</h1>

	<button onclick={carregar}>Listar hosts</button>

	{#if erro}
		<p class="erro">{erro}</p>
	{:else if hosts === null}
		<p>Nenhum host carregado ainda.</p>
	{:else if hosts.length === 0}
		<p>Nenhum host cadastrado.</p>
	{:else}
		<ul>
			{#each hosts as host (host.id)}
				<li>{host.name} — {host.addr}:{host.port} ({host.env})</li>
			{/each}
		</ul>
	{/if}
</main>

<style>
	main {
		font-family: system-ui, sans-serif;
		padding: 2rem;
	}

	.erro {
		color: #8e2b24;
	}
</style>
