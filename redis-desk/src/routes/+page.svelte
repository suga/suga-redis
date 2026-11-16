<script lang="ts">
	import ErroTecnico from "$lib/ErroTecnico.svelte";
	import { iniciarIdioma, t } from "$lib/i18n.svelte";
	/**
	 * A casca: barra do ambiente em cima, coluna de hosts à esquerda, área de
	 * trabalho à direita. A área de trabalho ganha a árvore de chaves e o
	 * painel de valor na Task 11.
	 */
	import { onMount } from "svelte";
	import ApprovalDialog from "$lib/ApprovalDialog.svelte";
	import Divisor from "$lib/Divisor.svelte";
	import EnvBand from "$lib/EnvBand.svelte";
	import HostForm from "$lib/HostForm.svelte";
	import HostList from "$lib/HostList.svelte";
	import UpdateBanner from "$lib/UpdateBanner.svelte";
	import Workspace from "$lib/Workspace.svelte";
	import { getVersion } from "@tauri-apps/api/app";
	import { procurar, type Achado } from "$lib/atualizacao";
	import {
		errorMessage,
		listHosts,
		onApprovalsChanged,
		pendingApprovals,
		type ApprovalRequest,
		type Host,
	} from "$lib/api";

	/**
	 * Largura da coluna de hosts antes de qualquer arrasto.
	 *
	 * É o TETO que a coluna tinha antes de virar arrastável
	 * (`minmax(14rem, 20rem)`), não o piso: com endereço de host longo — o
	 * caso normal num cluster gerenciado — o `minmax` assentava no topo, e
	 * partir do piso entregaria a quem abre o app pela primeira vez uma
	 * coluna ~30% mais estreita do que a de antes deste trabalho, cuja
	 * motivação declarada era que endereço e nome de chave vivem cortados.
	 */
	const LARGURA_HOSTS_PADRAO = 320; // 20rem
	let larguraHosts = $state(LARGURA_HOSTS_PADRAO);

	let hosts = $state<Host[]>([]);
	let aberto = $state<Host | null>(null);
	/** Motivo de uma falha de `connect` já sabida para o host aberto — repassado ao `Workspace`. */
	let falhaConexao = $state<string | null>(null);
	/**
	 * O caminho de volta: uma falha que a área de trabalho encontrou no host
	 * aberto, devolvida à coluna de hosts para a linha dele parar de dizer
	 * "conectado". Sem isto o estado era escrito no clique e nunca mais
	 * revisto — ver o comentário de `falhaExterna` em `HostList.svelte`.
	 */
	let falhaExterna = $state<{ hostId: string; motivo: string } | null>(null);
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

	// O idioma é decidido uma vez, na abertura: escolha guardada, senão o
	// idioma do sistema. Aqui e não no módulo porque `localStorage` só
	// existe depois que a janela existe.
	onMount(iniciarIdioma);

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

	// ------------------------------------------ versão nova no GitHub

	let atualizacao = $state<Achado | null>(null);
	let versaoAtual = $state("");

	// Uma vez, ao abrir. Não há relógio refazendo a pergunta: quem deixa o
	// app aberto o dia inteiro não precisa ser lembrado de hora em hora, e
	// a próxima abertura pergunta de novo de graça.
	onMount(async () => {
		const achado = await procurar();
		if (!achado) return;
		versaoAtual = await getVersion();
		atualizacao = achado;
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

	<div class="corpo" style="--largura-hosts: {larguraHosts}px">
		<aside>
			<HostList
				{hosts}
				selectedId={aberto?.id ?? null}
				{falhaExterna}
				onSelect={(host, motivo) => {
					aberto = host;
					falhaConexao = motivo ?? null;
					// Sem `motivo`, o `conectar` da coluna de hosts acabou de
					// dar certo: a falha que a área de trabalho tinha
					// reportado antes já não vale, e guardá-la aqui é um
					// "falhou" esperando para ressuscitar — basta que alguém
					// acrescente uma segunda dependência ao efeito que a lê
					// em `HostList.svelte`.
					if (!motivo) falhaExterna = null;
					editando = null;
				}}
				onNew={() => (editando = { host: null })}
				onEdit={(host) => (editando = { host })}
				onChanged={carregar}
				onDisconnected={(host) => {
					// Só fecha o workspace se era este o host aberto: desconectar
					// outro host, em segundo plano, não pode derrubar a tela de
					// quem está olhando.
					if (aberto?.id === host.id) {
						aberto = null;
						falhaConexao = null;
						falhaExterna = null;
					}
				}}
			/>
		</aside>

		<Divisor
			chave="largura:hosts"
			inicial={LARGURA_HOSTS_PADRAO}
			onLargura={(px) => (larguraHosts = px)}
		/>

		<main>
			{#if erro}
				<ErroTecnico assunto="erro.aoCarregarHosts" detalhe={erro} />
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
				{@const hostAberto = aberto}
				{#key hostAberto.id}
					<Workspace
						host={hostAberto}
						{falhaConexao}
						onFalhaDeConexao={(motivo) =>
							(falhaExterna = { hostId: hostAberto.id, motivo })}
					/>
				{/key}
			{:else}
				<!-- A marca só aparece aqui, onde não há nada competindo com ela.
				     No resto do app a cor forte é do ambiente, e nada disputa
				     atenção com ela. -->
				<div class="vazio">
					<img src="/suga-redis-logo.png" alt="Suga-Redis" width="340" />
					<p>{t("pagina.escolhaHost")}</p>
				</div>
			{/if}
		</main>
	</div>
</div>

<!-- Um pedido por vez: decidir dois ao mesmo tempo é decidir nenhum com
     atenção, e o resto da fila continua esperando sem risco. -->
{#if pedidos.length > 0}
	<ApprovalDialog request={pedidos[0]} onSettled={lerPedidos} />
{/if}

{#if atualizacao}
	<UpdateBanner
		achado={atualizacao}
		atual={versaoAtual}
		onDispensar={() => (atualizacao = null)}
	/>
{/if}

<style>
	.app {
		display: flex;
		flex-direction: column;
		height: 100vh;
	}

	.corpo {
		display: grid;
		/* A variável vem do `Divisor`: ele é quem mede o arrasto e lembra a
		   largura entre sessões. O valor fixo aqui só cobre o instante
		   antes da alça montar. */
		grid-template-columns: var(--largura-hosts, 20rem) 1fr;
		flex: 1;
		min-height: 0;
		/* Ancora o `Divisor`, que se posiciona sozinho em cima da borda
		   entre as colunas sem ocupar uma faixa própria na grade. */
		position: relative;
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
	main > :global(form),
	main > :global(.erro) {
		margin: 1.2rem;
	}

	.vazio {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 1.2rem;
		height: 100%;
		padding: 2rem;
		text-align: center;
	}

	.vazio img {
		max-width: min(340px, 70%);
		height: auto;
		/* A arte tem fundo preto próprio; misturá-la ao painel evita a
		   plaquinha retangular que ela seria em cima do fundo do app. */
		mix-blend-mode: lighten;
	}

	.vazio p {
		margin: 0;
		color: var(--text-faint);
	}


	@media (max-width: 40rem) {
		.corpo {
			grid-template-columns: 1fr;
			grid-template-rows: auto 1fr;
		}

		/* Sem colunas lado a lado, não há borda para a alça marcar. */
		.corpo :global(.divisor) {
			display: none;
		}
	}
</style>
