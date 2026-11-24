<script lang="ts">
	import ErroTecnico from "$lib/ErroTecnico.svelte";
	/**
	 * A área de trabalho de um host aberto: escolha do banco, árvore de
	 * chaves à esquerda, painel do valor à direita.
	 */
	import ConfirmDialog from "$lib/ConfirmDialog.svelte";
	import ContextMenu from "$lib/ContextMenu.svelte";
	import type { Item } from "$lib/ContextMenu.svelte";
	import DeletePrefixDialog from "$lib/DeletePrefixDialog.svelte";
	import Divisor from "$lib/Divisor.svelte";
	import KeyEditor from "$lib/KeyEditor.svelte";
	import KeyTree from "$lib/KeyTree.svelte";
	import HistoryPanel from "$lib/HistoryPanel.svelte";
	import LogPanel from "$lib/LogPanel.svelte";
	import NewKeyDialog from "$lib/NewKeyDialog.svelte";
	import SearchPanel from "$lib/SearchPanel.svelte";
	import ValuePanel from "$lib/ValuePanel.svelte";
	import { haQuantoTempo, numero } from "$lib/formato";
	import { t } from "$lib/i18n.svelte";
	import { resumoDaOperacao } from "$lib/resumo";
	import {
		cancelDelete,
		deletePrefix,
		enumeratePrefix,
		errorMessage,
		isApiError,
		listDbs,
		onBulkDeleteProgress,
		onBulkScanProgress,
		onRescanProgress,
		onTreeProgress,
		scanTree,
		rescan,
		scanStatus,
		textBytes,
		treeLevel,
		writeValue,
		type BackupRequest,
		type EnumerationHandle,
		type Host,
		type ScanProgress,
		type Branch,
		type ScanSnapshot,
		type ScanReport,
		type WriteOp,
	} from "$lib/api";
	import { onMount, untrack } from "svelte";

	interface Props {
		host: Host;
		/**
		 * O motivo de uma falha de `connect` já observada por quem abriu
		 * este host — `null` quando a conexão deu certo, ou quando o
		 * workspace foi aberto sem passar por `connect` (clique no nome).
		 *
		 * Lido direto do prop, NUNCA copiado para um `$state` próprio: uma
		 * nova tentativa no MESMO host (falhou, e a pessoa clicou Conectar
		 * de novo) precisa refletir aqui sem exigir remontagem — e um
		 * `$state` inicializado a partir de um prop só captura o valor
		 * inicial, o mesmo problema que `docs/CONTEXTO.md` registra para
		 * `db` mais abaixo.
		 */
		falhaConexao: string | null;
		/**
		 * Avisa quem abriu este host que uma operação não alcançou o
		 * servidor. É o caminho de volta do estado de conexão: sem ele a
		 * linha do host continuava dizendo "conectado · 12 ms" enquanto
		 * esta área de trabalho mostrava "não foi possível conectar" —
		 * duas afirmações contrárias sobre o mesmo banco, na mesma tela.
		 *
		 * Opcional: quem monta o `Workspace` fora da casca (os testes) não
		 * precisa se importar com a coluna de hosts.
		 */
		onFalhaDeConexao?: (motivo: string) => void;
	}

	let { host, falhaConexao, onFalhaDeConexao }: Props = $props();

	/**
	 * Largura da coluna de chaves antes de qualquer arrasto — o TETO que a
	 * coluna tinha antes de virar arrastável (`minmax(16rem, 24rem)`), e
	 * não o piso. Ver a mesma nota em `+page.svelte`: nome de chave longo
	 * fazia o `minmax` assentar no topo, e partir do piso estreitaria a
	 * coluna justamente para quem nunca arrastou nada.
	 */
	const LARGURA_CHAVES_PADRAO = 384; // 24rem
	let larguraChaves = $state(LARGURA_CHAVES_PADRAO);

	/**
	 * O banco parte do que o host traz cadastrado e passa a ser escolha da
	 * pessoa — recarregar a lista de hosts no fundo não pode puxá-la de
	 * volta para o banco do cadastro. Trocar de host é remontagem, feita com
	 * `{#key}` por quem usa este componente.
	 */
	let db = $state(untrack(() => host.db));
	let bancos = $state<[number, number][]>([]);
	let filtro = $state("");
	let selecionada = $state<string | null>(null);
	let relatorio = $state<ScanReport | null>(null);
	/**
	 * Falha ao perguntar o relatório de índice — distinta de `relatorio ===
	 * null`, que também é a resposta legítima de um banco nunca varrido.
	 * Sem isto, as duas viravam o mesmo "nunca varrido" na tela.
	 */
	let erroIndice = $state("");
	/**
	 * A pergunta ao índice está em voo — distinta de "nunca varrido", que é
	 * a resposta legítima quando a pergunta JÁ voltou e não há relatório.
	 * Sem isto, um host lento (a própria origem do incidente: 5679 ms só
	 * para um PING) mostrava "nunca varrido" enquanto ainda esperava a
	 * resposta — os mesmos pixels de uma pergunta respondida.
	 */
	let perguntandoIndice = $state(false);
	/**
	 * Sobe a cada nova pergunta ao índice. Uma troca rápida de banco dispara
	 * uma pergunta nova antes da anterior voltar; se a antiga responder
	 * depois, seu `.then`/`.catch` compara a geração que carregava consigo
	 * contra esta e desiste — sem isso, uma resposta atrasada de um banco
	 * que não é mais o aberto sobrescrevia o estado do banco atual.
	 */
	let geracaoIndice = 0;
	/**
	 * O mesmo para a varredura da árvore, com contador PRÓPRIO: "Varrer de
	 * novo" sobe `geracaoIndice` sem mexer na árvore, e usar um contador só
	 * faria um clique no botão descartar a falha de uma varredura que ainda
	 * é a do banco aberto.
	 */
	let geracaoArvore = 0;
	/**
	 * Falha ao listar os bancos — distinta de `bancos.length === 0`, que
	 * também é a resposta legítima de um host sem nenhum banco populado.
	 * Sem isto, o seletor ficava pelado nos dois casos.
	 */
	let erroBancos = $state("");
	let progresso = $state<ScanProgress | null>(null);
	/** Andamento da varredura que monta a árvore. */
	let lendo = $state<ScanSnapshot | null>(null);
	/** Os prefixos de primeiro nível, crescendo a cada retrato. */
	let raiz = $state<Branch[]>([]);
	/** Sobe a cada retrato: é o que manda a árvore reler os galhos abertos. */
	let versaoArvore = $state(0);
	let varrendo = $state(false);
	let erro = $state("");
	let criando = $state(false);
	/** Escrita de chave única recusada em produção, esperando confirmação. */
	let escritaPendente = $state<{ key: string; op: WriteOp; summary: string } | null>(null);
	/** Força o painel a reler a chave depois de uma escrita. */
	let revisao = $state(0);

	// ------------------------------------------- menu de contexto e exclusão

	/** Sobre o que o menu aberto fala. Uma chave não é um prefixo. */
	type AlvoMenu =
		| { tipo: "prefixos"; prefixos: string[] }
		| { tipo: "chave"; chave: string };

	let menu = $state<{ x: number; y: number; alvo: AlvoMenu } | null>(null);

	/**
	 * A lista de itens é montada AQUI, não dentro do `ContextMenu` — ele é
	 * genérico e não sabe o que é um prefixo.
	 *
	 * Busca e estatística não entram desabilitadas: não entram. Um item
	 * cinza que nunca acende ensina a pessoa a parar de abrir o menu, e aí
	 * os itens que funcionam somem junto.
	 *
	 * O item destrutivo fica por último, e é o único com `perigoso` — a
	 * marca é um traço acima dele, que só faz sentido com algo acima.
	 */
	let itensDoMenu = $derived.by<Item[]>(() => {
		if (!menu) return [];
		if (menu.alvo.tipo === "chave") {
			return [
				{ id: "copiar-nome", rotulo: t("menu.copiarNome") },
				{ id: "apagar-chave", rotulo: t("menu.apagarChave"), perigoso: true },
			];
		}
		const n = menu.alvo.prefixos.length;
		return [
			{
				id: "copiar-prefixo",
				rotulo: n === 1 ? t("menu.copiarPrefixo") : t("menu.copiarPrefixos", { n: numero(n) }),
			},
			{
				id: "apagar-prefixo",
				rotulo: n === 1 ? t("menu.apagarPrefixo") : t("menu.apagarPrefixos", { n: numero(n) }),
				perigoso: true,
			},
		];
	});

	/** A exclusão em massa aberta na tela. Só uma por vez, por contrato. */
	interface Apagar {
		prefixos: string[];
		/** `null` enquanto a enumeração ainda corre. */
		enumeracao: EnumerationHandle | null;
		progresso: { scanned: number; total: number } | null;
		/** Não-nulo enquanto as chaves saem: quantas já saíram. */
		apagados: number | null;
		/** Qual escolha de cópia o Rust acabou de recusar por tamanho. */
		copiaRecusada: BackupRequest | null;
	}
	let apagar = $state<Apagar | null>(null);

	/**
	 * Falha ao enumerar — estado PRÓPRIO, como `erroBancos` e `erroIndice`.
	 *
	 * Sem ele, uma pergunta que não alcançou o servidor viraria "não há nada
	 * sob este prefixo": a mesma troca de uma falha por uma resposta vazia
	 * que já custou caro duas vezes neste arquivo.
	 */
	let erroEnumerar = $state("");
	/**
	 * A falha que não é da enumeração, com o ASSUNTO dela junto.
	 *
	 * O assunto viaja com a mensagem porque três coisas diferentes caem
	 * neste slot, e a frase de cima é o que a pessoa lê para decidir o que
	 * fazer. Dizer "não deu para apagar as chaves" depois de uma cópia para
	 * a área de transferência que falhou — ou, pior, depois de uma exclusão
	 * que DEU certo e só não conseguiu redesenhar a árvore — mandaria
	 * alguém procurar um estrago que não houve, ou desmentiria um ato que
	 * aconteceu. Mesmo padrão de `assuntoErro` em `HistoryPanel`.
	 */
	type AssuntoApagar =
		| "erro.aoApagarPrefixo"
		| "erro.aoCopiar"
		| "erro.aoDesistir"
		| "erro.aoRelerArvore";
	let erroApagar = $state("");
	let assuntoApagar = $state<AssuntoApagar>("erro.aoApagarPrefixo");

	function falhouNoApagar(assunto: AssuntoApagar, err: unknown) {
		assuntoApagar = assunto;
		erroApagar = errorMessage(err);
	}
	/** O desfecho de um apagar — ou o aviso de que não havia nada para apagar. */
	let avisoApagar = $state("");
	/**
	 * Sobe a cada pedido de exclusão. Uma resposta atrasada de um pedido que
	 * já foi substituído (ou desistido) não pode aterrissar na tela — mesma
	 * régua de `geracaoIndice` e `geracaoArvore`.
	 */
	let geracaoApagar = 0;

	type Aba = "valor" | "busca" | "historico" | "logs";
	let aba = $state<Aba>("valor");
	const ABAS = [
		{ id: "valor", chave: "aba.valor" },
		{ id: "busca", chave: "aba.busca" },
		{ id: "historico", chave: "aba.historico" },
		{ id: "logs", chave: "aba.logs" },
	] as const satisfies { id: Aba; chave: Parameters<typeof t>[0] }[];

	onMount(() => {
		const pararVarredura = onRescanProgress((p) => (progresso = p));
		const pararLeitura = onTreeProgress((retrato) => {
			// O evento é global e a varredura do Rust não morre com a tela:
			// desconectar um host no meio da varredura dele e abrir outro
			// em seguida fazia os retratos do primeiro — que continuam
			// chegando — plantarem as chaves dele na árvore do segundo. O
			// desmonte do `{#key}` cobre fechar a área de trabalho, não
			// trocar de host enquanto algo ainda corre.
			if (retrato.host_id !== host.id || retrato.db !== db) return;
			raiz = retrato.root;
			versaoArvore += 1;
			// `done` apaga a barra, mas a raiz fica: ela É a árvore.
			lendo = retrato.done ? null : retrato;
		});
		// Os dois eventos da exclusão em massa não vêm endereçados (o Rust
		// manda só os números), e não precisam: só existe uma exclusão por
		// vez na tela, e sem `apagar` aberto não há onde o número pousar.
		const pararVarreduraEmMassa = onBulkScanProgress((tick) => {
			if (apagar) {
				apagar.progresso = { scanned: tick.scanned, total: tick.total_estimate };
			}
		});
		const pararExclusao = onBulkDeleteProgress((saidas) => {
			// Só enquanto as chaves de fato saem: `apagados` é a trava contra o
			// segundo clique, e deixá-la nascer de um evento a acenderia por
			// um andamento de outra exclusão qualquer.
			if (apagar && apagar.apagados !== null) apagar.apagados = saidas;
		});

		return () => {
			pararVarredura.then((fn) => fn());
			pararLeitura.then((fn) => fn());
			pararVarreduraEmMassa.then((fn) => fn());
			pararExclusao.then((fn) => fn());
		};
	});

	/** Quanto da leitura já veio, de 0 a 100. */
	let porcentagem = $derived(
		lendo && lendo.total_estimate > 0
			? Math.min(100, Math.round((lendo.scanned / lendo.total_estimate) * 100))
			: 0,
	);

	/**
	 * Dispara a varredura do banco e NÃO espera por ela.
	 *
	 * Os retratos chegam por evento, a raiz aparece em menos de um segundo
	 * e os galhos já abrem com o que foi lido até ali — a varredura segue
	 * preenchendo o resto em segundo plano. Esperá-la terminar antes de
	 * desenhar seria deixar a tela muda por quase um minuto num banco de
	 * milhões de chaves.
	 */
	async function abrirBanco(hostId: string, banco: number) {
		erro = "";
		erroIndice = "";
		// Os avisos da exclusão falam do banco que estava aberto: "1.200
		// chaves apagadas" é verdade, mas sobre outro banco, e deixá-lo na
		// tela do banco novo o torna uma frase falsa sem mudar uma palavra.
		erroEnumerar = "";
		erroApagar = "";
		avisoApagar = "";
		relatorio = null;
		perguntandoIndice = true;
		selecionada = null;
		raiz = [];
		lendo = { scanned: 0, total_estimate: 0, root: [], done: false };

		const minhaGeracao = ++geracaoIndice;
		const minhaVarredura = ++geracaoArvore;
		scanStatus(hostId, banco)
			.then((r) => {
				// Uma troca de banco mais nova já partiu: esta resposta é de
				// uma pergunta que não interessa mais mostrar.
				if (minhaGeracao !== geracaoIndice) return;
				relatorio = r;
				erroIndice = "";
				perguntandoIndice = false;
			})
			.catch((err) => {
				if (minhaGeracao !== geracaoIndice) return;
				relatorio = null;
				erroIndice = errorMessage(err);
				perguntandoIndice = false;
			});

		try {
			await scanTree(hostId, banco);
		} catch (err) {
			// Mesma régua da pergunta de índice logo acima: uma varredura
			// mais nova já partiu (troca de banco) ou a conexão já foi
			// sabida como falha, e esta resposta atrasada não pode trocar o
			// que está na tela por um motivo de um banco que já não é o
			// aberto.
			if (minhaVarredura !== geracaoArvore) return;
			erro = errorMessage(err);
			// A varredura é a primeira coisa que este host faz depois de
			// aberto: se ela não alcançou o servidor, a linha do host na
			// coluna ao lado não pode seguir dizendo "conectado". O
			// `erroBancos` não avisa ninguém por fora de propósito — ele
			// tem slot visível aqui mesmo, chega em paralelo com este, e
			// dois avisos fariam a linha do host alternar entre dois jeitos
			// de contar a mesma queda.
			onFalhaDeConexao?.(erro);
		} finally {
			// Também sob a geração: limpar `lendo` de uma varredura velha
			// apagaria a barra de progresso da varredura nova.
			if (minhaVarredura === geracaoArvore) {
				lendo = null;
			}
		}
	}

	/**
	 * Uma conexão já sabida como falha não ganha uma segunda tentativa: sem
	 * isto, o `scan_tree` do Rust abre o próprio socket por conta própria e
	 * repete, em silêncio, a mesma falha que a linha do host já mostra —
	 * deixando a tela dizer "falhou" e "lendo as chaves" ao mesmo tempo, a
	 * própria confusão que esta tarefa existe para acabar.
	 */
	$effect(() => {
		if (falhaConexao) {
			erro = falhaConexao;
			erroIndice = "";
			perguntandoIndice = false;
			geracaoIndice += 1; // descarta qualquer pergunta de índice em voo
			geracaoArvore += 1; // e qualquer varredura que ainda esteja correndo
			selecionada = null;
			raiz = [];
			lendo = null;
			relatorio = null;
			return;
		}
		abrirBanco(host.id, db);
	});

	$effect(() => {
		const hostId = host.id;
		erroBancos = "";
		listDbs(hostId)
			.then((lista) => {
				bancos = lista;
				erroBancos = "";
			})
			.catch((err) => {
				bancos = [];
				erroBancos = errorMessage(err);
			});
	});

	async function varrer() {
		varrendo = true;
		progresso = null;
		erro = "";
		// Simétrico ao `erro` acima: sem isto, um "não foi possível
		// perguntar" de antes do clique sobrevivia debaixo de uma linha que
		// o próprio "Varrer de novo" acabou de corrigir. A geração também
		// sobe, para que uma resposta atrasada da pergunta de índice do
		// `abrirBanco` anterior não sobrescreva o relatório que este
		// `rescan` está prestes a trazer.
		erroIndice = "";
		perguntandoIndice = false;
		const minhaGeracao = ++geracaoIndice;
		const minhaVarredura = ++geracaoArvore;
		try {
			// Primeiro a ÁRVORE, depois o índice.
			//
			// Quem clica aqui quer ver o que mudou no banco — chaves novas,
			// chaves que sumiram — e isso é a varredura de nomes, não o
			// índice de valores. É ela também que alimenta a barra de
			// progresso (`lendo`, por `onTreeProgress`), a mesma que
			// aparece ao abrir um banco; o índice segue mostrando a própria
			// contagem na linha de cima.
			lendo = { scanned: 0, total_estimate: 0, root: [], done: false };
			await scanTree(host.id, db);
			if (minhaVarredura !== geracaoArvore) return;
			// Os galhos abertos releem da trie recém-refeita.
			versaoArvore += 1;

			const novoRelatorio = await rescan(host.id, db);
			// O seletor desliga durante a varredura (ver a nota de design
			// logo acima do markup), mas o estado da tela ainda pode mudar
			// por outros caminhos enquanto isto corre — uma falha de
			// conexão sabida, por exemplo, que sobe `geracaoIndice`. Este
			// resultado, de uma pergunta que já não é a atual, não pode
			// aterrissar.
			if (minhaGeracao !== geracaoIndice) return;
			relatorio = novoRelatorio;
		} catch (err) {
			if (minhaGeracao !== geracaoIndice) return;
			erro = errorMessage(err);
		} finally {
			if (minhaVarredura === geracaoArvore) lendo = null;
			varrendo = false;
			progresso = null;
		}
	}

	/**
	 * O Redis não guarda hash, lista ou sorted set vazios: uma chave desses
	 * tipos só passa a existir com ao menos um elemento dentro. Por isso a
	 * criação já entra com um elemento de partida, que a pessoa edita ou
	 * remove em seguida — a alternativa seria uma chave que some sozinha
	 * assim que a tela fosse recarregada.
	 */
	function opDeCriacao(kind: "string" | "hash" | "list" | "zset"): WriteOp {
		switch (kind) {
			case "string":
				return { op: "set_string", value: textBytes("") };
			case "hash":
				return { op: "set_hash_field", field: "campo", value: textBytes("") };
			case "list":
				return { op: "push_list_item", value: textBytes(""), front: false };
			case "zset":
				return { op: "set_zset_member", member: textBytes("membro"), score: 0 };
		}
	}

	async function criar(nova: { key: string; kind: "string" | "hash" | "list" | "zset" }) {
		const op = opDeCriacao(nova.kind);
		criando = false;
		await aplicarEscrita(nova.key, op, false);
	}

	/**
	 * Uma escrita de chave ÚNICA — criar aqui, apagar pelo menu de contexto.
	 *
	 * A exclusão em massa não passa por aqui: ela tem outra trava (o nome do
	 * host digitado), outra cópia de segurança e uma linha de histórico só
	 * dela. Mandar uma chave solta por lá seria pedir o nome do host para
	 * apagar uma chave.
	 */
	async function aplicarEscrita(key: string, op: WriteOp, confirmado: boolean) {
		erro = "";
		try {
			const resultado = await writeValue(host.id, db, key, op, confirmado);
			if (resultado.outcome === "needs_confirmation") {
				escritaPendente = { key, op, summary: resultado.summary };
				return;
			}
			escritaPendente = null;
			if (op.op === "delete_key") {
				// A chave não existe mais: tirá-la da seleção e mandar a árvore
				// reler, como `chaveSumiu` faz quando o painel a encontra morta.
				if (selecionada === key) selecionada = null;
				versaoArvore += 1;
			} else {
				selecionada = key;
			}
		} catch (err) {
			escritaPendente = null;
			erro = errorMessage(err);
		}
	}

	// ----------------------------------------- menu de contexto da árvore

	/**
	 * Põe texto na área de transferência.
	 *
	 * Falhar aqui não é motivo para derrubar nada: um navegador que recusa o
	 * acesso deixa a pessoa sem a cópia, não sem a árvore. O que não pode é
	 * o `catch` ficar mudo — a frase vai para o mesmo slot dos outros avisos.
	 */
	async function copiar(texto: string) {
		try {
			await navigator.clipboard.writeText(texto);
		} catch (err) {
			falhouNoApagar("erro.aoCopiar", err);
		}
	}

	function escolherNoMenu(id: string) {
		const alvo = menu?.alvo;
		// O menu sai da frente antes da ação: uma ação que já aconteceu não
		// fica com o menu aberto por cima dela.
		menu = null;
		if (!alvo) return;

		if (alvo.tipo === "chave") {
			if (id === "copiar-nome") copiar(alvo.chave);
			if (id === "apagar-chave") aplicarEscrita(alvo.chave, { op: "delete_key" }, false);
			return;
		}
		if (id === "copiar-prefixo") copiar(alvo.prefixos.join("\n"));
		if (id === "apagar-prefixo") comecarApagar(alvo.prefixos);
	}

	// ------------------------------------------------- exclusão em massa

	/**
	 * Confere o que há sob os prefixos e abre o diálogo com o número de
	 * agora.
	 *
	 * A contagem da árvore é de uma varredura antiga, e a decisão de apagar
	 * não pode pesar um número velho — é essa diferença que faz a enumeração
	 * ser um passo à parte.
	 *
	 * Uma recusa por teto de cópia NÃO passa por aqui: a enumeração
	 * sobrevive a ela na fachada, e o diálogo que já está na tela só troca
	 * as opções de cópia (ver `confirmarApagar`). Varrer de novo ali seria
	 * cobrar a operação mais cara do aplicativo por uma escolha que a
	 * pessoa ainda nem refez.
	 */
	async function comecarApagar(prefixos: string[]) {
		erroEnumerar = "";
		erroApagar = "";
		avisoApagar = "";
		apagar = {
			prefixos,
			enumeracao: null,
			progresso: null,
			apagados: null,
			copiaRecusada: null,
		};

		const minha = ++geracaoApagar;
		try {
			const achado = await enumeratePrefix(host.id, db, prefixos);
			if (minha !== geracaoApagar || !apagar) return;

			if (achado.count === 0) {
				// Consentimento para apagar zero chaves é consentimento para um
				// não-evento, e a linha de histórico descreveria nada. As chaves
				// expiraram entre a árvore e a varredura; dizer isso é a resposta
				// inteira.
				apagar = null;
				avisoApagar = t("apagarPrefixo.nadaAqui");
				return;
			}
			apagar.enumeracao = achado;
		} catch (err) {
			if (minha !== geracaoApagar) return;
			// NUNCA "não há nada sob este prefixo": uma pergunta que falhou e
			// uma resposta vazia são coisas diferentes, e confundi-las é o
			// defeito que este projeto já pagou duas vezes.
			apagar = null;
			erroEnumerar = errorMessage(err);
		}
	}

	async function confirmarApagar(backup: BackupRequest, nomeDigitado: string | null) {
		const atual = apagar;
		if (!atual?.enumeracao) return;
		// A trava contra o segundo clique mora AQUI, e não no diálogo: ele
		// precisa seguir usável depois de uma recusa por nome, para a pessoa
		// digitar e repetir sem pagar outra varredura. Sem esta linha, dois
		// cliques rápidos mandam a mesma enumeração duas vezes e a segunda
		// volta `UnknownEnumeration`.
		if (atual.apagados !== null) return;
		atual.apagados = 0;

		const id = atual.enumeracao.id;
		erroApagar = "";
		try {
			const resultado = await deletePrefix(id, backup, nomeDigitado);
			if (apagar !== atual) return;

			if (resultado.outcome === "needs_confirmation") {
				// Nada foi apagado e a enumeração continua valendo: destravar o
				// botão é o bastante, e o nome digitado segue na caixa.
				atual.apagados = null;
				return;
			}
			if (resultado.outcome === "deleted") {
				apagar = null;
				avisoApagar = resumoDoApagado(resultado.deleted, resultado.enumerated);
				depoisDeApagar(atual.prefixos);
				return;
			}
			// Nenhum outro desfecho vem desta porta hoje. Se um dia vier,
			// fechar é o pior que pode acontecer — deixar o diálogo na tela
			// com o botão travado em `apagados = 0` e nada a clicar seria
			// uma janela da qual só se sai pelo Escape, sobre uma operação
			// de desfecho desconhecido.
			apagar = null;
		} catch (err) {
			if (apagar !== atual) return;

			if (isApiError(err) && err.kind === "backup_too_large") {
				// Nenhuma chave saiu: o Rust lê os retratos ANTES de apagar e
				// recusa inteiro quando eles passam do teto. Isto não é uma
				// falha a mostrar e pronto — é a escolha da cópia voltando com
				// uma opção a menos. Apagar com uma cópia que não coube não é
				// uma terceira saída.
				//
				// A contagem continua valendo: a fachada a devolve ao mapa
				// nesta recusa, sob o mesmo id, exatamente como já fazia com
				// `needs_confirmation`. Por isso aqui não se varre de novo —
				// o diálogo que já está na tela só muda de opções.
				atual.apagados = null;
				atual.copiaRecusada = backup;
				return;
			}
			apagar = null;
			falhouNoApagar("erro.aoApagarPrefixo", err);
		}
	}

	/** A frase do desfecho: o que saiu, e se saiu tudo que foi prometido. */
	function resumoDoApagado(saidas: number, prometidas: number): string {
		if (saidas < prometidas) {
			return t("apagarPrefixo.feitoParcial", {
				feitas: numero(saidas),
				prometidas: numero(prometidas),
			});
		}
		const chave = saidas === 1 ? "apagarPrefixo.feitoUma" : "apagarPrefixo.feitoVarias";
		return t(chave, { n: numero(saidas) });
	}

	/**
	 * Põe a árvore de acordo com o banco depois de uma exclusão.
	 *
	 * Os galhos abertos releem sozinhos de `versaoArvore`, mas a RAIZ não:
	 * ela é desenhada dos retratos da varredura, que já terminou. Sem reler
	 * o nível de cima, o prefixo apagado continuaria listado com a contagem
	 * que tinha antes — a tela mostrando o que não existe mais.
	 */
	function depoisDeApagar(prefixos: string[]) {
		if (selecionada && prefixos.some((prefixo) => selecionada?.startsWith(prefixo))) {
			selecionada = null;
		}
		versaoArvore += 1;
		treeLevel(host.id, db, "")
			.then((nivel) => {
				if (nivel) raiz = nivel.children;
			})
			.catch((err) => {
				// A exclusão deu certo; o que falhou foi redesenhar. Dizer que
				// falhou "o apagar" desmentiria o ato, e as chaves apagadas
				// continuam apagadas — por isso o assunto é outro, e o
				// desfecho em `avisoApagar` fica onde está, inteiro.
				falhouNoApagar("erro.aoRelerArvore", err);
			});
	}

	/**
	 * Escape, clique fora e o botão Cancelar chegam todos aqui.
	 *
	 * Antes de a exclusão começar, fechar é só fechar. Depois que as chaves
	 * começaram a sair, fechar a janela esconderia uma exclusão que continua
	 * correndo — então aqui o gesto vira DESISTIR, e o diálogo fica de pé
	 * mostrando o andamento até ele parar.
	 */
	function cancelarApagar() {
		const atual = apagar;
		if (!atual) return;

		if (atual.apagados !== null && atual.enumeracao) {
			cancelDelete(atual.enumeracao.id).catch((err) => {
				// Assunto PRÓPRIO, e nunca o de apagar: o pedido de parada é
				// que não chegou — a exclusão continua correndo e provavelmente
				// vai terminar. Dizer "não deu para apagar as chaves" aqui
				// desmentiria o ato no instante exato em que ele acontece, que
				// é o mesmo erro que `erro.aoRelerArvore` existe para evitar,
				// só que de trás para frente.
				falhouNoApagar("erro.aoDesistir", err);
			});
			return;
		}
		// Descarta uma enumeração que ainda esteja em voo: sem isto, ela
		// voltaria e reabriria o diálogo que a pessoa acabou de fechar.
		geracaoApagar += 1;
		apagar = null;
	}

	/** Uma escrita foi aplicada: relê a chave e atualiza a árvore. */
	function escreveu() {
		revisao += 1;
		// Apagar é escrita como qualquer outra, e o núcleo já tirou a chave
		// da trie. A árvore na tela precisa reler os galhos abertos para
		// deixar de mostrar o que não existe mais.
		versaoArvore += 1;
	}

	/**
	 * A chave sumiu entre a varredura e o clique: tira da seleção e manda a
	 * árvore reler.
	 *
	 * O núcleo já a removeu da trie ao descobrir, na leitura, que ela não
	 * existe mais. Sem releitura aqui, ela continuaria listada e clicar
	 * nela só repetiria o mesmo erro.
	 */
	function chaveSumiu() {
		selecionada = null;
		versaoArvore += 1;
	}
</script>

<div class="trabalho" style="--largura-chaves: {larguraChaves}px">
	<div class="coluna">
		<div class="controles">
			<!--
				Nota de design: o seletor desliga enquanto a varredura MANUAL
				corre (`varrendo`), e só ela. `progresso` (do evento global
				`onRescanProgress`, sem `host_id`/`db` no payload) e o próprio
				`varrendo` não carregam geração nenhuma — não dá para
				descartar uma resposta atrasada que não se sabe de qual banco
				é, e ali a pessoa escolheu esperar ao clicar em "Varrer de
				novo". A varredura AUTOMÁTICA, que parte sozinha a cada troca
				de banco, NÃO desliga o seletor: ela é a primeira coisa que
				acontece num banco recém-aberto e, num banco de milhões de
				chaves, congelaria o controle por quase um minuto sem
				ninguém ter pedido nada. O que ela pode deixar correndo —
				a pergunta de índice do `scanStatus` e a varredura da árvore
				— está protegido por `geracaoIndice` e `geracaoArvore`, que
				comparam a geração da resposta contra a atual antes de
				escrever qualquer coisa na tela.
			-->
			<label class="banco">
				<span>{t("campo.banco")}</span>
				<select bind:value={db} disabled={varrendo}>
					{#if bancos.length === 0}
						<option value={db}>{db}</option>
					{:else}
						{#each bancos as [indice, chaves] (indice)}
							<option value={indice}>{t("banco.opcao", { indice, chaves: numero(chaves) })}</option>
						{/each}
					{/if}
				</select>
			</label>
			{#if erroBancos}
				<ErroTecnico assunto="erro.aoListarBancos" detalhe={erroBancos} testid="erro-bancos" />
			{/if}

			<div class="busca-linha">
				<input
					class="filtro"
					bind:value={filtro}
					placeholder={t("filtro.placeholder")}
					aria-label={t("filtro.rotulo")}
				/>
				<button onclick={() => (criando = true)}>{t("acao.novaChave")}</button>
			</div>

			<div class="varredura">
				<span class="idade">
					{#if varrendo && progresso}
						{t("varredura.andamento", {
							lidas: numero(progresso.scanned),
							total: numero(progresso.total_estimate),
						})}
					{:else if varrendo}
						{t("varredura.semTotal")}
					{:else if relatorio}
						{t("indice.idade", {
							quando: haQuantoTempo(
								Math.max(0, Math.round((Date.now() - relatorio.finished_at) / 1000)),
							),
						})}
					{:else if erroIndice}
						{t("indice.naoDeu")}
					{:else if perguntandoIndice}
						{t("indice.perguntando")}
					{:else}
						{t("indice.nunca")}
					{/if}
				</span>
				<button onclick={varrer} disabled={varrendo}>{t("acao.varrerDeNovo")}</button>
			</div>
			{#if erroIndice}
				<ErroTecnico assunto="erro.aoLerIndice" detalhe={erroIndice} testid="erro-indice" />
			{/if}

			<!--
				Os avisos da exclusão em massa ficam AQUI, ao lado da árvore de
				que falam, e não no painel da direita: é a árvore que mudou (ou
				deixou de mudar), e é para ela que a pessoa está olhando.
				Cada falha tem o seu, como `erroBancos` e `erroIndice`.
			-->
			{#if erroEnumerar}
				<ErroTecnico
					assunto="erro.aoEnumerarPrefixo"
					detalhe={erroEnumerar}
					testid="erro-enumerar"
				/>
			{/if}
			{#if erroApagar}
				<ErroTecnico assunto={assuntoApagar} detalhe={erroApagar} testid="erro-apagar" />
			{/if}
			{#if avisoApagar}
				<p class="aviso-apagar" role="status" data-testid="aviso-apagar">{avisoApagar}</p>
			{/if}
		</div>

		{#if lendo}
			<div class="lendo" data-testid="lendo">
				<p class="lendo-linha">
					{t("leitura.emSegundoPlano")}
					{#if lendo.total_estimate > 0}
						<span class="mono">
							{porcentagem}% ({numero(lendo.scanned)} de {numero(lendo.total_estimate)})
						</span>
					{/if}
				</p>
				<div
					class="barra"
					role="progressbar"
					aria-valuenow={porcentagem}
					aria-valuemin="0"
					aria-valuemax="100"
					aria-label={t("leitura.rotulo")}
				>
					<div class="barra-cheia" style="width: {porcentagem}%"></div>
				</div>
			</div>
		{/if}

		<div class="arvore">
			<KeyTree
				hostId={host.id}
				{db}
				root={raiz}
				version={versaoArvore}
				selectedKey={selecionada}
				onSelect={(key) => (selecionada = key)}
				filter={filtro}
				onPrefixMenu={(prefixos, x, y) => (menu = { x, y, alvo: { tipo: "prefixos", prefixos } })}
				onKeyMenu={(chave, x, y) => (menu = { x, y, alvo: { tipo: "chave", chave } })}
			/>
		</div>
	</div>

	<Divisor
		chave="largura:chaves"
		inicial={LARGURA_CHAVES_PADRAO}
		onLargura={(px) => (larguraChaves = px)}
	/>

	<div class="painel">
		<nav class="abas">
			{#each ABAS as item (item.id)}
				<button
					class="aba"
					class:ativa={aba === item.id}
					aria-current={aba === item.id ? "page" : undefined}
					onclick={() => (aba = item.id)}
				>
					{t(item.chave)}
				</button>
			{/each}
		</nav>

		<div class="conteudo">
		{#if aba === "busca"}
			<SearchPanel
				hostId={host.id}
				{db}
				onOpenKey={(key) => {
					selecionada = key;
					aba = "valor";
				}}
			/>
		{:else if aba === "historico"}
			<HistoryPanel hostId={host.id} onRolledBack={escreveu} />
		{:else if aba === "logs"}
			<LogPanel />
		{:else if erro}
			<ErroTecnico assunto="erro.aoLerChave" detalhe={erro} />
		{:else if selecionada}
			{#key `${selecionada}#${revisao}`}
				<ValuePanel
					hostId={host.id}
					{db}
					keyName={selecionada}
					onKeyGone={chaveSumiu}
				>
					{#snippet editor(view)}
						<KeyEditor hostId={host.id} {db} {view} onWritten={escreveu} />
					{/snippet}
				</ValuePanel>
			{/key}
		{:else}
			<p class="aviso">{t("arvore.escolhaChave")}</p>
		{/if}
		</div>
	</div>
</div>

{#if criando}
	<NewKeyDialog onCreate={criar} onCancel={() => (criando = false)} />
{/if}

{#if escritaPendente}
	<ConfirmDialog
		summary={resumoDaOperacao(escritaPendente.key, escritaPendente.op)}
		onConfirm={() =>
			escritaPendente &&
			aplicarEscrita(escritaPendente.key, escritaPendente.op, true)}
		onCancel={() => (escritaPendente = null)}
	/>
{/if}

{#if menu}
	<ContextMenu
		x={menu.x}
		y={menu.y}
		itens={itensDoMenu}
		onEscolher={escolherNoMenu}
		onFechar={() => (menu = null)}
	/>
{/if}

{#if apagar}
	<DeletePrefixDialog
		{host}
		{db}
		prefixos={apagar.prefixos}
		enumeracao={apagar.enumeracao}
		progresso={apagar.progresso}
		apagados={apagar.apagados}
		copiaRecusada={apagar.copiaRecusada}
		onConfirmar={confirmarApagar}
		onCancelar={cancelarApagar}
	/>
{/if}

<style>
	.trabalho {
		display: grid;
		/* A variável vem do `Divisor`: ele é quem mede o arrasto e lembra a
		   largura entre sessões. O valor fixo aqui só cobre o instante
		   antes da alça montar. */
		grid-template-columns: var(--largura-chaves, 24rem) 1fr;
		height: 100%;
		min-height: 0;
		/* Ancora o `Divisor`, que se posiciona sozinho em cima da borda
		   entre as colunas sem ocupar uma faixa própria na grade. */
		position: relative;
	}

	.coluna {
		display: flex;
		flex-direction: column;
		min-height: 0;
		border-right: 1px solid var(--rule);
	}

	.controles {
		display: flex;
		flex-direction: column;
		gap: 0.45rem;
		padding: 0.6rem 0.7rem;
		border-bottom: 1px solid var(--rule);
	}

	.banco {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}

	.banco > span {
		color: var(--text-dim);
		font-size: 0.9rem;
	}

	.varredura {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
	}

	.idade {
		color: var(--text-faint);
		font-size: 0.88rem;
	}

	.varredura button {
		padding: 0.12rem 0.45rem;
		font-size: 0.88rem;
		color: var(--text-dim);
	}

	.busca-linha {
		display: flex;
		gap: 0.4rem;
	}

	.busca-linha button {
		white-space: nowrap;
	}

	.arvore {
		flex: 1;
		min-height: 0;
	}

	.painel {
		display: flex;
		flex-direction: column;
		min-width: 0;
		min-height: 0;
	}

	.abas {
		display: flex;
		gap: 0.2rem;
		padding: 0 1.2rem;
		border-bottom: 1px solid var(--rule);
	}

	.aba {
		padding: 0.45rem 0.7rem;
		border: none;
		border-radius: 0;
		border-bottom: 2px solid transparent;
		color: var(--text-faint);
	}

	.aba:hover {
		color: var(--text-dim);
	}

	.ativa {
		color: var(--text);
		border-bottom-color: var(--action);
	}

	.conteudo {
		flex: 1;
		min-height: 0;
		padding: 1rem 1.2rem;
		overflow-y: auto;
	}

	.aviso {
		margin: 0;
		padding: 0.8rem 0.2rem;
		color: var(--text-faint);
	}

	/* O desfecho de uma exclusão é prosa de corpo: ele conta o que acabou
	   de acontecer com dados de produção, e não é nota de rodapé. Sem cor
	   nenhuma — o vermelho `--prod` significa só o ambiente. */
	.aviso-apagar {
		margin: 0;
		color: var(--text);
		font-size: 0.9rem;
	}

	.lendo {
		padding: 0.8rem;
	}

	.lendo-linha {
		margin: 0 0 0.45rem;
		color: var(--text-dim);
		font-size: 0.92rem;
	}

	/* Barra fina, sem animação própria: ela já se move sozinha conforme os
	   lotes chegam, e um brilho correndo por cima só competiria com isso. */
	.barra {
		height: 3px;
		background: var(--panel-dark);
		border-radius: 999px;
		overflow: hidden;
	}

	.barra-cheia {
		height: 100%;
		background: var(--action);
		transition: width 120ms linear;
	}


	@media (max-width: 52rem) {
		.trabalho {
			grid-template-columns: 1fr;
			grid-template-rows: minmax(12rem, 40%) 1fr;
		}

		.coluna {
			border-right: none;
			border-bottom: 1px solid var(--rule);
		}

		/* Sem colunas lado a lado, não há borda para a alça marcar. */
		.trabalho :global(.divisor) {
			display: none;
		}
	}
</style>
