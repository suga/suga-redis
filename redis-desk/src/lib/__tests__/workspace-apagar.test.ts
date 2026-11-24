/**
 * O fio que liga o clique direito na árvore ao histórico.
 *
 * Nada aqui é comportamento novo: a varredura, o apagar em lotes, a volta
 * atrás, as duas chamadas da fachada, o menu genérico e o diálogo já
 * existem e já têm teste próprio. O que estes testes cobram é a COSTURA —
 * que a escolha do menu alcance a enumeração, que a enumeração alcance o
 * diálogo, que a confirmação alcance o Rust com a escolha de cópia que a
 * pessoa fez, e que cada jeito de dar errado tenha um lugar visível para
 * aparecer.
 *
 * O perigo desta tarefa é o `catch` silencioso, que já aconteceu duas vezes
 * neste arquivo (`listDbs` e `scanStatus`): uma pergunta que falhou virava
 * uma resposta vazia, e um banco que não pôde ser lido ficava igual a um
 * banco sem chaves. Por isso o teste da falha de enumeração não se contenta
 * em ver o diálogo fechado — ele cobra a mensagem na tela.
 */
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";

vi.mock("$lib/api", async (importOriginal) => {
	const real = await importOriginal<typeof import("$lib/api")>();
	return {
		...real,
		listDbs: vi.fn(async () => [] as [number, number][]),
		onRescanProgress: vi.fn(async () => () => {}),
		onTreeProgress: vi.fn(async () => () => {}),
		onBulkScanProgress: vi.fn(async () => () => {}),
		onBulkDeleteProgress: vi.fn(async () => () => {}),
		onBulkBackupProgress: vi.fn(async () => () => {}),
		scanTree: vi.fn(async () => undefined),
		rescan: vi.fn(),
		scanStatus: vi.fn(async () => null),
		treeLevel: vi.fn(async () => null),
		loadMoreChildren: vi.fn(),
		writeValue: vi.fn(),
		enumeratePrefix: vi.fn(),
		deletePrefix: vi.fn(),
		cancelDelete: vi.fn(async () => true),
		cancelEnumeration: vi.fn(async () => true),
	};
});

import {
	cancelDelete,
	cancelEnumeration,
	deletePrefix,
	enumeratePrefix,
	onBulkBackupProgress,
	onBulkDeleteProgress,
	onTreeProgress,
	treeLevel,
	writeValue,
} from "$lib/api";
import Workspace from "$lib/Workspace.svelte";
import type {
	Branch,
	EnumerationHandle,
	Host,
	TreeProgress,
	WriteOutcome,
} from "$lib/api";

const enumerou = vi.mocked(enumeratePrefix);
const apagou = vi.mocked(deletePrefix);
const desistiu = vi.mocked(cancelDelete);
const desistiuDaVarredura = vi.mocked(cancelEnumeration);
const escreveu = vi.mocked(writeValue);
const nivel = vi.mocked(treeLevel);
const assinouArvore = vi.mocked(onTreeProgress);
const assinouApagar = vi.mocked(onBulkDeleteProgress);

function host(partes: Partial<Host> = {}): Host {
	return {
		id: "h1",
		name: "Redis de testes",
		addr: "127.0.0.1",
		port: 6379,
		db: 0,
		username: null,
		tls: false,
		env: "dev",
		...partes,
	};
}

function galho(label: string, count: number): Branch {
	return { label, full_prefix: `${label}:`, count, leaf_key: null };
}

function folha(label: string): Branch {
	return { label, full_prefix: `${label}:`, count: 1, leaf_key: label };
}

function enumeracao(partes: Partial<EnumerationHandle> = {}): EnumerationHandle {
	return {
		id: "enum-1",
		count: 1_200,
		kinds: [["string", 1_200]],
		truncated: false,
		unrestorable: 0,
		...partes,
	};
}

/** A raiz que a árvore desenha: dois prefixos e uma chave solta. */
const RAIZ: Branch[] = [galho("sessao", 1_200), galho("pedido", 40), folha("solta")];

/**
 * Monta o workspace e entrega a raiz pelo mesmo caminho do app: o evento de
 * retrato da varredura. Montar a árvore por outra porta testaria um estado
 * que o produto nunca alcança.
 */
async function montar(partes: Partial<Host> = {}) {
	let entregar!: (retrato: TreeProgress) => void;
	assinouArvore.mockImplementation(async (handler) => {
		entregar = handler;
		return () => {};
	});

	const alvo = host(partes);
	render(Workspace, { props: { host: alvo, falhaConexao: null } });

	await waitFor(() => expect(entregar).toBeDefined());
	entregar({
		host_id: alvo.id,
		db: alvo.db,
		scanned: 1_241,
		total_estimate: 1_241,
		root: RAIZ,
		done: true,
	});

	await screen.findByRole("button", { name: /sessao/ });
	return alvo;
}

/** Abre o menu de contexto sobre a linha cujo rótulo casa. */
async function menuSobre(rotulo: RegExp) {
	const user = userEvent.setup();
	const linha = await screen.findByRole("button", { name: rotulo });
	await user.pointer({ keys: "[MouseRight]", target: linha });
	return screen.getByRole("menu");
}

/** Abre o menu no prefixo `sessao:` e escolhe o item de apagar. */
async function pedirApagarPrefixo() {
	const user = userEvent.setup();
	const menu = await menuSobre(/sessao/);
	await user.click(within(menu).getByRole("menuitem", { name: /^Apagar tudo/ }));
}

beforeEach(() => {
	vi.clearAllMocks();
	// `clearAllMocks` zera as CHAMADAS, não a fila dos `...Once`: um
	// `mockResolvedValueOnce` que o teste anterior não chegou a consumir
	// vaza para o próximo e atende a primeira chamada dele, por baixo da
	// implementação que ele acabou de instalar. Custou um "liberar is not a
	// function" sem relação nenhuma com o que estava sendo testado.
	enumerou.mockReset();
	apagou.mockReset();
	escreveu.mockReset();

	nivel.mockImplementation(async () => null);
	enumerou.mockImplementation(async () => enumeracao());
	apagou.mockImplementation(async () => ({
		outcome: "deleted",
		entry_id: 9,
		deleted: 1_200,
		attempted: 1_200,
		enumerated: 1_200,
	}));
	desistiu.mockImplementation(async () => true);
	desistiuDaVarredura.mockReset();
	desistiuDaVarredura.mockImplementation(async () => true);
	assinouApagar.mockImplementation(async () => () => {});
});

// --------------------------------------------------------------- o menu

test("o menu de prefixo traz Apagar tudo e Copiar prefixo, e nada cinza", async () => {
	await montar();
	const menu = await menuSobre(/sessao/);

	const itens = within(menu).getAllByRole("menuitem");
	expect(itens.map((b) => b.textContent?.trim())).toEqual([
		"Copiar prefixo",
		"Apagar tudo sob este prefixo…",
	]);

	// Item cinza que nunca acende ensina a ignorar o menu: busca e
	// estatística não entram desabilitadas, não entram de jeito nenhum.
	for (const item of itens) expect(item).toBeEnabled();
	expect(within(menu).queryByRole("menuitem", { name: /Buscar/ })).toBeNull();
	expect(within(menu).queryByRole("menuitem", { name: /Estatística/ })).toBeNull();
});

test("o menu de chave traz Copiar nome e Apagar chave", async () => {
	await montar();
	const menu = await menuSobre(/solta/);

	expect(within(menu).getAllByRole("menuitem").map((b) => b.textContent?.trim())).toEqual([
		"Copiar nome",
		"Apagar chave",
	]);
});

test("as reticências marcam quem tem diálogo próprio, e só esse", async () => {
	await montar();

	// A reticência é contrato: ela promete um diálogo QUE O ITEM POSSUI.
	// "Apagar tudo sob este prefixo" abre o seu em qualquer ambiente.
	const prefixo = await menuSobre(/sessao/);
	expect(
		within(prefixo).getByRole("menuitem", { name: /^Apagar tudo/ }).textContent,
	).toMatch(/…$/);
	expect(within(prefixo).getByRole("menuitem", { name: "Copiar prefixo" }).textContent)
		.not.toMatch(/…$/);

	await userEvent.setup().keyboard("{Escape}");

	// "Apagar chave" NÃO possui diálogo nenhum: o que aparece em produção é
	// a trava de produção, que está na frente de toda escrita deste
	// aplicativo. Reivindicá-la com reticências faria o item prometer o que
	// não é dele — e, pela mesma régua, todo controle que escreve teria de
	// carregar uma.
	const chave = await menuSobre(/solta/);
	for (const item of within(chave).getAllByRole("menuitem")) {
		expect(item.textContent).not.toMatch(/…$/);
	}
});

test("Copiar prefixo põe o prefixo na área de transferência", async () => {
	const user = userEvent.setup();
	await montar();

	const menu = await menuSobre(/sessao/);
	await user.click(within(menu).getByRole("menuitem", { name: "Copiar prefixo" }));

	expect(await navigator.clipboard.readText()).toBe("sessao:");
	// E o menu sai da frente: uma ação que já aconteceu não fica aberta.
	expect(screen.queryByRole("menu")).toBeNull();
});

test("Copiar nome põe o nome da chave na área de transferência", async () => {
	const user = userEvent.setup();
	await montar();

	const menu = await menuSobre(/solta/);
	await user.click(within(menu).getByRole("menuitem", { name: "Copiar nome" }));

	// O nome da chave, não o prefixo acumulado com os dois-pontos no fim.
	expect(await navigator.clipboard.readText()).toBe("solta");
});

test("Apagar chave usa o writeValue que já existe, não a porta nova", async () => {
	const user = userEvent.setup();
	escreveu.mockResolvedValue({ outcome: "applied", entry_id: 3 });
	const alvo = await montar();

	const menu = await menuSobre(/solta/);
	await user.click(within(menu).getByRole("menuitem", { name: "Apagar chave" }));

	await waitFor(() =>
		expect(escreveu).toHaveBeenCalledWith(alvo.id, 0, "solta", { op: "delete_key" }, false),
	);
	// A exclusão em massa é outro caminho, com outra trava e outra linha de
	// histórico: uma chave só nunca passa por ela.
	expect(enumerou).not.toHaveBeenCalled();
	expect(apagou).not.toHaveBeenCalled();
});

// ------------------------------------------------- do menu até o diálogo

test("escolher o item do menu chama enumeratePrefix e abre o diálogo", async () => {
	const alvo = await montar();
	await pedirApagarPrefixo();

	expect(enumerou).toHaveBeenCalledWith(alvo.id, 0, ["sessao:"]);

	const dialogo = await screen.findByRole("dialog");
	// A contagem do diálogo é a da enumeração (1.200), conferida agora — e
	// não a que a árvore mostra na linha.
	expect(dialogo).toHaveTextContent("1.200");
	expect(within(dialogo).getByText("sessao:")).toBeInTheDocument();
});

test("não abre o diálogo para uma enumeração vazia, e diz que não há nada ali", async () => {
	enumerou.mockResolvedValue(enumeracao({ count: 0 }));
	await montar();
	await pedirApagarPrefixo();

	// Pedir consentimento para apagar zero chaves é pedir consentimento para
	// um não-evento — e gravaria uma linha de histórico que não descreve nada.
	expect(await screen.findByTestId("aviso-apagar")).toHaveTextContent(
		/nenhuma chave sob/i,
	);
	expect(screen.queryByRole("dialog")).toBeNull();
	expect(apagou).not.toHaveBeenCalled();
});

test("uma falha ao enumerar aparece na tela, não vira banco vazio", async () => {
	enumerou.mockRejectedValue({
		kind: "unreachable",
		message: "não foi possível alcançar o host",
	});
	await montar();
	await pedirApagarPrefixo();

	expect(await screen.findByTestId("erro-enumerar")).toHaveTextContent(
		"não foi possível alcançar o host",
	);
	// A falha NÃO pode se disfarçar de "não há nada sob este prefixo": é
	// exatamente a confusão que custou caro neste arquivo.
	expect(screen.queryByTestId("aviso-apagar")).toBeNull();
	expect(screen.queryByRole("dialog")).toBeNull();
});

// ----------------------------------------------------------- o confirmar

test("confirmar chama deletePrefix com a escolha de cópia", async () => {
	const user = userEvent.setup();
	await montar();
	await pedirApagarPrefixo();

	const dialogo = await screen.findByRole("dialog");
	await user.click(within(dialogo).getByRole("button", { name: "Apagar 1.200 chaves" }));

	expect(apagou).toHaveBeenCalledWith("enum-1", "inline", null);
});

test("desmarcar a cópia chega ao Rust como apagar sem cópia", async () => {
	const user = userEvent.setup();
	await montar();
	await pedirApagarPrefixo();

	const dialogo = await screen.findByRole("dialog");
	await user.click(
		within(dialogo).getByLabelText("Guardar uma cópia no histórico antes de apagar"),
	);
	await user.click(
		within(dialogo).getByRole("button", { name: "Apagar 1.200 chaves sem volta atrás" }),
	);

	expect(apagou).toHaveBeenCalledWith("enum-1", "none", null);
});

test("needs_confirmation reabre o diálogo pedindo o nome digitado", async () => {
	const user = userEvent.setup();
	apagou.mockResolvedValueOnce({
		outcome: "needs_confirmation",
		summary: "Apagar 1.200 chaves sob sessao:",
	});
	apagou.mockResolvedValueOnce({
		outcome: "deleted",
		entry_id: 9,
		deleted: 1_200,
		attempted: 1_200,
		enumerated: 1_200,
	});
	await montar({ env: "prod", name: "Redis de produção" });
	await pedirApagarPrefixo();

	const dialogo = await screen.findByRole("dialog");
	const campo = within(dialogo).getByLabelText("Digite o nome do host para apagar");
	const botao = () => within(dialogo).getByRole("button", { name: "Apagar 1.200 chaves" });

	await user.type(campo, "Redis de produção");
	await user.click(botao());
	expect(apagou).toHaveBeenLastCalledWith("enum-1", "inline", "Redis de produção");

	// A recusa não apagou nada e não consumiu a contagem. O diálogo tem que
	// voltar USÁVEL — se a trava do segundo clique ficasse presa aqui, a
	// pessoa teria que fechar tudo e pagar outra varredura para tentar de
	// novo, que é exatamente o que a fachada se deu ao trabalho de evitar.
	await waitFor(() => expect(botao()).toBeEnabled());
	await user.click(botao());

	await waitFor(() => expect(apagou).toHaveBeenCalledTimes(2));
	expect(apagou).toHaveBeenLastCalledWith("enum-1", "inline", "Redis de produção");
	expect(enumerou).toHaveBeenCalledTimes(1);
});

test("dois cliques seguidos em apagar mandam uma chamada só", async () => {
	const user = userEvent.setup();
	let liberar!: (saida: WriteOutcome) => void;
	apagou.mockImplementation(
		() => new Promise<WriteOutcome>((resolve) => (liberar = resolve)),
	);
	await montar();
	await pedirApagarPrefixo();

	const dialogo = await screen.findByRole("dialog");
	const botao = within(dialogo).getByRole("button", { name: "Apagar 1.200 chaves" });

	// Cliques SÍNCRONOS, sem flush entre eles: um dedo nervoso não espera o
	// repintar, e com `await` quem recusaria o segundo seria o atributo
	// `disabled` já no DOM, que é uma barreira mais fraca que a testada.
	//
	// O que este teste prova é o CONTRATO observável — duas chamadas nunca
	// saem com a mesma enumeração —, não qual das duas travas o cumpriu.
	// São duas, de propósito: a daqui (`apagados !== null`, em
	// `confirmarApagar`) e a do diálogo, que não é uma bandeira própria mas
	// o `liberado = !apagando` derivado do mesmo prop. Tirar uma deixa a
	// outra de pé, então nenhum clique as distingue; a de cá existe porque
	// `Workspace` não pode depender da derivação interna de um filho para
	// uma propriedade que custa um `UnknownEnumeration`. O que pinta a
	// ordem — `apagados = 0` ANTES da chamada — é o teste seguinte.
	fireEvent.click(botao);
	fireEvent.click(botao);
	await waitFor(() => expect(apagou).toHaveBeenCalled());

	expect(apagou).toHaveBeenCalledTimes(1);
	liberar({
		outcome: "deleted",
		entry_id: 9,
		deleted: 1_200,
		attempted: 1_200,
		enumerated: 1_200,
	});
});

test("o diálogo entra em modo apagando antes do primeiro andamento chegar", async () => {
	const user = userEvent.setup();
	// Nenhum evento de andamento é entregue neste teste, de propósito.
	apagou.mockImplementation(() => new Promise<WriteOutcome>(() => {}));
	await montar();
	await pedirApagarPrefixo();

	const dialogo = await screen.findByRole("dialog");
	const botao = within(dialogo).getByRole("button", { name: "Apagar 1.200 chaves" });
	await user.click(botao);

	// `apagados` já vale 0 — marcado ANTES de pedir a exclusão, não quando
	// o primeiro `bulk://delete-progress` chegar. Entre o clique e esse
	// primeiro evento cabe uma ida ao servidor inteira, e deixar a trava
	// nascer lá é deixar essa janela aberta para o segundo clique.
	expect(within(dialogo).getByRole("status")).toHaveTextContent("0");
	expect(botao).toBeDisabled();
});

// --------------------------------------------- a cópia que não coube

test("BackupTooLarge reabre o diálogo oferecendo gravar em arquivo", async () => {
	const user = userEvent.setup();
	apagou.mockRejectedValueOnce({
		kind: "backup_too_large",
		message: "a cópia de segurança da exclusão em massa passou do teto de 67108864 bytes",
	});
	await montar();
	await pedirApagarPrefixo();

	const dialogo = await screen.findByRole("dialog");
	await user.click(within(dialogo).getByRole("button", { name: "Apagar 1.200 chaves" }));

	// Não vira erro cru: volta como uma escolha, com a razão à vista.
	const reaberto = await screen.findByRole("dialog");
	await waitFor(() =>
		expect(
			within(reaberto).getByLabelText("Gravar uma cópia em arquivo antes de apagar"),
		).toBeChecked(),
	);
	expect(
		within(reaberto).queryByLabelText("Guardar uma cópia no histórico antes de apagar"),
	).toBeNull();
	expect(reaberto).toHaveTextContent(/não coube/i);
	expect(screen.queryByTestId("erro-apagar")).toBeNull();

	// E NÃO paga outra varredura: a fachada devolve a contagem ao mapa
	// nesta recusa, então o mesmo id continua valendo. Varrer de novo aqui
	// cobraria a operação mais cara do aplicativo por uma escolha que a
	// pessoa ainda nem refez.
	expect(enumerou).toHaveBeenCalledTimes(1);

	await user.click(within(reaberto).getByRole("button", { name: "Apagar 1.200 chaves" }));
	expect(apagou).toHaveBeenLastCalledWith("enum-1", "file", null);
});

test("o caminho inteiro das duas recusas: nada apagado, uma varredura só", async () => {
	const user = userEvent.setup();
	apagou.mockRejectedValueOnce({
		kind: "backup_too_large",
		message: "a cópia de segurança da exclusão em massa passou do teto de 67108864 bytes",
	});
	apagou.mockRejectedValueOnce({
		kind: "backup_too_large",
		message: "a cópia de segurança da exclusão em massa passou do teto de 67108864 bytes",
	});
	await montar();
	await pedirApagarPrefixo();

	await user.click(
		within(await screen.findByRole("dialog")).getByRole("button", {
			name: "Apagar 1.200 chaves",
		}),
	);
	const comArquivo = await screen.findByRole("dialog");
	await waitFor(() =>
		expect(
			within(comArquivo).getByLabelText("Gravar uma cópia em arquivo antes de apagar"),
		).toBeInTheDocument(),
	);
	await user.click(within(comArquivo).getByRole("button", { name: "Apagar 1.200 chaves" }));

	// Nem a embutida nem o arquivo couberam. Oferecer o arquivo uma terceira
	// vez seria oferecer de novo o que acabou de ser recusado; apagar com
	// uma cópia que não coube não é saída nenhuma. Sobram duas.
	const ultimo = await screen.findByRole("dialog");
	await waitFor(() => expect(ultimo).toHaveTextContent(/nem em arquivo/i));
	expect(
		within(ultimo).queryByLabelText("Gravar uma cópia em arquivo antes de apagar"),
	).toBeNull();
	expect(
		within(ultimo).queryByLabelText("Guardar uma cópia no histórico antes de apagar"),
	).toBeNull();
	expect(
		within(ultimo).getByRole("button", { name: "Apagar 1.200 chaves sem volta atrás" }),
	).toBeEnabled();
	expect(within(ultimo).getByRole("button", { name: "Cancelar" })).toBeEnabled();

	// Em nenhum ponto do caminho uma chave saiu. As duas provas disso na
	// tela: nenhum desfecho foi anunciado, e a árvore nunca foi relida —
	// a releitura da raiz só acontece depois de um apagar que aconteceu.
	expect(screen.queryByTestId("aviso-apagar")).toBeNull();
	expect(nivel).not.toHaveBeenCalledWith("h1", 0, "");
	// E em nenhum ponto a pessoa pagou uma segunda varredura.
	expect(enumerou).toHaveBeenCalledTimes(1);
	expect(apagou.mock.calls.map((c) => c[0])).toEqual(["enum-1", "enum-1"]);
});

// ------------------------------------------------------- depois do apagar

test("terminado o apagar, a árvore é relida", async () => {
	const user = userEvent.setup();
	const alvo = await montar();
	await pedirApagarPrefixo();

	const dialogo = await screen.findByRole("dialog");
	nivel.mockClear();
	await user.click(within(dialogo).getByRole("button", { name: "Apagar 1.200 chaves" }));

	// A raiz é desenhada dos retratos da varredura, não da trie: sem reler,
	// `sessao` continuaria na tela marcando 1.200 chaves que já não existem.
	await waitFor(() => expect(nivel).toHaveBeenCalledWith(alvo.id, 0, ""));
	expect(screen.queryByRole("dialog")).toBeNull();
	expect(await screen.findByTestId("aviso-apagar")).toHaveTextContent("1.200");
});

test("desistir no meio do apagar pede para parar, em vez de só fechar a janela", async () => {
	const user = userEvent.setup();
	let entregarSaidas!: (quantas: number) => void;
	assinouApagar.mockImplementation(async (handler) => {
		entregarSaidas = handler;
		return () => {};
	});
	apagou.mockImplementation(() => new Promise<WriteOutcome>(() => {}));
	await montar();
	await pedirApagarPrefixo();

	const dialogo = await screen.findByRole("dialog");
	await user.click(within(dialogo).getByRole("button", { name: "Apagar 1.200 chaves" }));

	entregarSaidas(400);
	expect(await within(dialogo).findByRole("status")).toHaveTextContent("400");

	await user.keyboard("{Escape}");

	// Fechar a janela esconderia uma exclusão que continua correndo: Escape
	// no meio do ato é desistir dele.
	await waitFor(() => expect(desistiu).toHaveBeenCalledWith("enum-1"));
	expect(screen.getByRole("dialog")).toBeInTheDocument();
});

test("uma falha ao apagar aparece na tela", async () => {
	const user = userEvent.setup();
	apagou.mockRejectedValue({
		kind: "unreachable",
		message: "a conexão caiu no meio",
	});
	await montar();
	await pedirApagarPrefixo();

	const dialogo = await screen.findByRole("dialog");
	await user.click(within(dialogo).getByRole("button", { name: "Apagar 1.200 chaves" }));

	expect(await screen.findByTestId("erro-apagar")).toHaveTextContent("a conexão caiu no meio");
	expect(screen.queryByRole("dialog")).toBeNull();
});

// ----------------------------- o que vem depois não desmente o que houve

test("uma falha ao copiar não diz que o apagar falhou", async () => {
	const user = userEvent.setup();
	await montar();

	// Um navegador que recusa a área de transferência (contexto sem HTTPS,
	// permissão negada) faz `writeText` estourar.
	vi.spyOn(navigator.clipboard, "writeText").mockRejectedValue(
		new Error("clipboard indisponível"),
	);

	const menu = await menuSobre(/sessao/);
	await user.click(within(menu).getByRole("menuitem", { name: "Copiar prefixo" }));

	const aviso = await screen.findByTestId("erro-apagar");
	expect(aviso).toHaveTextContent("clipboard indisponível");
	// Nada foi apagado, e dizer "não deu para apagar as chaves" sobre uma
	// cópia que falhou mandaria a pessoa procurar um estrago que não houve.
	expect(aviso).not.toHaveTextContent(/apagar as chaves/);
	expect(aviso).toHaveTextContent(/copiar/i);
	expect(apagou).not.toHaveBeenCalled();
});

test("uma releitura que falha depois do apagar não desmente o apagar", async () => {
	const user = userEvent.setup();
	await montar();
	await pedirApagarPrefixo();

	// As chaves saíram; o que quebrou foi desenhar a árvore de novo.
	nivel.mockRejectedValue({ kind: "unreachable", message: "a conexão caiu depois" });

	const dialogo = await screen.findByRole("dialog");
	await user.click(within(dialogo).getByRole("button", { name: "Apagar 1.200 chaves" }));

	// O desfecho continua dito, inteiro: "o que vem depois do ato não pode
	// desmentir o ato". A falha da releitura entra ao lado, com o nome dela.
	expect(await screen.findByTestId("aviso-apagar")).toHaveTextContent("1.200");
	const aviso = await screen.findByTestId("erro-apagar");
	expect(aviso).toHaveTextContent("a conexão caiu depois");
	expect(aviso).not.toHaveTextContent(/não deu para apagar/i);
	expect(aviso).toHaveTextContent(/Varrer de novo/);
});

test("o menu segue o segundo clique direito, em vez de ficar onde abriu", async () => {
	const user = userEvent.setup();
	await montar();

	const sessao = await screen.findByRole("button", { name: /sessao/ });
	const solta = await screen.findByRole("button", { name: /solta/ });

	await user.pointer({ keys: "[MouseRight]", target: sessao, coords: { x: 10, y: 20 } });
	expect(screen.getByRole("menu").getAttribute("style")).toContain("left: 10px");

	// O `ContextMenu` calcula a viragem UMA vez, na montagem — a janela não
	// muda debaixo dele. Se a segunda abertura reaproveitasse a instância,
	// o menu apareceria ancorado no clique anterior, falando da linha nova.
	await user.pointer({ keys: "[MouseRight]", target: solta, coords: { x: 300, y: 400 } });
	const menu = screen.getByRole("menu");
	expect(menu.getAttribute("style")).toContain("left: 300px");
	expect(within(menu).getByRole("menuitem", { name: "Copiar nome" })).toBeInTheDocument();
});


// --------------------------------------------- vários prefixos de uma vez

test("marcados vários prefixos, o menu fala no plural e leva todos", async () => {
	const user = userEvent.setup();
	const alvo = await montar();

	const sessao = await screen.findByRole("button", { name: /sessao/ });
	const pedido = await screen.findByRole("button", { name: /pedido/ });
	await user.keyboard("{Meta>}");
	await user.click(sessao);
	await user.click(pedido);
	await user.keyboard("{/Meta}");

	await user.pointer({ keys: "[MouseRight]", target: pedido });
	const menu = screen.getByRole("menu");

	// É o caso de manchete desta função, e o rótulo mora no item
	// destrutivo: ele precisa dizer QUANTOS prefixos vão junto.
	expect(menu.textContent).toContain("Copiar os 2 prefixos");
	expect(menu.textContent).toContain("Apagar tudo sob os 2 prefixos marcados…");

	await user.click(within(menu).getByRole("menuitem", { name: /^Apagar tudo/ }));

	expect(enumerou).toHaveBeenCalledWith(alvo.id, 0, ["sessao:", "pedido:"]);
	const dialogo = await screen.findByRole("dialog");
	expect(within(dialogo).getByText("sessao:")).toBeInTheDocument();
	expect(within(dialogo).getByText("pedido:")).toBeInTheDocument();
});

test("copiar vários prefixos põe um por linha", async () => {
	const user = userEvent.setup();
	await montar();

	const sessao = await screen.findByRole("button", { name: /sessao/ });
	const pedido = await screen.findByRole("button", { name: /pedido/ });
	await user.keyboard("{Meta>}");
	await user.click(sessao);
	await user.click(pedido);
	await user.keyboard("{/Meta}");

	await user.pointer({ keys: "[MouseRight]", target: sessao });
	await user.click(
		within(screen.getByRole("menu")).getByRole("menuitem", { name: "Copiar os 2 prefixos" }),
	);

	expect(await navigator.clipboard.readText()).toBe("sessao:\npedido:");
});

// ------------------------------------------------- a desistência que falha

test("uma desistência que falha não diz que o apagar falhou", async () => {
	const user = userEvent.setup();

	// Primeiro uma falha de OUTRO assunto, para provar que o assunto não
	// sobra: sem reatribuí-lo, a tela diria "não deu para copiar" por cima
	// de um erro de cancelamento.
	vi.spyOn(navigator.clipboard, "writeText").mockRejectedValue(new Error("sem clipboard"));
	await montar();
	const menuCopiar = await menuSobre(/sessao/);
	await user.click(within(menuCopiar).getByRole("menuitem", { name: "Copiar prefixo" }));
	await screen.findByTestId("erro-apagar");

	apagou.mockImplementation(() => new Promise<WriteOutcome>(() => {}));
	desistiu.mockRejectedValue({
		kind: "unreachable",
		message: "o pedido de parada não chegou",
	});
	await pedirApagarPrefixo();
	const dialogo = await screen.findByRole("dialog");
	await user.click(within(dialogo).getByRole("button", { name: "Apagar 1.200 chaves" }));
	await user.keyboard("{Escape}");

	const aviso = await screen.findByTestId("erro-apagar");
	expect(aviso).toHaveTextContent("o pedido de parada não chegou");
	// A exclusão NÃO falhou — ela continua correndo, e provavelmente vai
	// terminar. Dizer "não deu para apagar as chaves" aqui desmente o ato
	// no instante exato em que ele acontece.
	expect(aviso).not.toHaveTextContent(/não deu para apagar as chaves/i);
	expect(aviso).not.toHaveTextContent(/copiar/i);
	expect(aviso).toHaveTextContent(/continua correndo/i);
});


// ------------------------------------- o que a tela diz quando sobra chave

test("a frase do parcial é medida pelo TENTADO, não pelo que o servidor contou", async () => {
	const user = userEvent.setup();
	// `deleted` menor que `enumerated` sem nada ter parado é rotina: uma
	// chave que expirou sozinha entre a contagem e o `UNLINK`, ou um reenvio
	// que perdeu a conta de um `UNLINK` que o servidor já tinha aplicado.
	// Dizer "o resto continua no banco" em cima desse número é afirmar algo
	// falso sobre o banco — e é uma frase sobre a qual alguém vai agir.
	apagou.mockResolvedValue({
		outcome: "deleted",
		entry_id: 9,
		deleted: 300,
		attempted: 1_200,
		enumerated: 1_200,
	});
	await montar();
	await pedirApagarPrefixo();

	const dialogo = await screen.findByRole("dialog");
	await user.click(within(dialogo).getByRole("button", { name: "Apagar 1.200 chaves" }));

	const aviso = await screen.findByTestId("aviso-apagar");
	expect(aviso).not.toHaveTextContent(/parou antes do fim/i);
	expect(aviso).not.toHaveTextContent(/resto continua no banco/i);
	expect(aviso).toHaveTextContent("300");
});

test("uma exclusão que parou de verdade diz que o resto continua no banco", async () => {
	const user = userEvent.setup();
	apagou.mockResolvedValue({
		outcome: "deleted",
		entry_id: 9,
		deleted: 500,
		attempted: 500,
		enumerated: 1_200,
	});
	await montar();
	await pedirApagarPrefixo();

	const dialogo = await screen.findByRole("dialog");
	await user.click(within(dialogo).getByRole("button", { name: "Apagar 1.200 chaves" }));

	const aviso = await screen.findByTestId("aviso-apagar");
	expect(aviso).toHaveTextContent("500 de 1.200");
	expect(aviso).toHaveTextContent(/resto continua no banco/i);
});

test("com a varredura cortada no teto, o desfecho manda repetir a operação", async () => {
	const user = userEvent.setup();
	// O menu prometeu "Apagar tudo sob este prefixo…"; a varredura parou no
	// teto, então sobrou chave. O que salvava isto hoje era acidental — a
	// trie ainda mostra o galho —, e acidente não é aviso.
	enumerou.mockImplementation(async () =>
		enumeracao({ count: 100_000, truncated: true }),
	);
	apagou.mockResolvedValue({
		outcome: "deleted",
		entry_id: 9,
		deleted: 100_000,
		attempted: 100_000,
		enumerated: 100_000,
	});
	await montar();
	await pedirApagarPrefixo();

	const dialogo = await screen.findByRole("dialog");
	await user.click(within(dialogo).getByRole("button", { name: "Apagar 100.000 chaves" }));

	const aviso = await screen.findByTestId("aviso-apagar");
	expect(aviso).toHaveTextContent("100.000");
	expect(aviso).toHaveTextContent(/ainda há chaves sob este prefixo/i);
	expect(aviso).toHaveTextContent(/repita a operação/i);
});

// ------------------------------------------- desistir durante a varredura

test("fechar a janela durante a varredura manda a varredura parar", async () => {
	const user = userEvent.setup();
	// Fechar só escondia a janela: o cursor seguia correndo no Rust até dar
	// a volta no espaço de chaves inteiro, porque o `MATCH` do `SCAN` não
	// encurta o trabalho do servidor. Quem clicou no prefixo errado não
	// tinha como parar de pagar por isso.
	enumerou.mockImplementation(() => new Promise<EnumerationHandle | null>(() => {}));
	const alvo = await montar();
	await pedirApagarPrefixo();

	await screen.findByRole("dialog");
	await user.keyboard("{Escape}");

	await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
	expect(desistiuDaVarredura).toHaveBeenCalledWith(alvo.id, alvo.db);
	// E não o desistir da EXCLUSÃO: nenhuma chave chegou a sair, e não há
	// id de exclusão nenhum a parar.
	expect(desistiu).not.toHaveBeenCalled();
});

test("uma varredura interrompida fecha a janela sem inventar frase nenhuma", async () => {
	// `null` é o que a fachada devolve quando a varredura foi interrompida a
	// pedido: não é falha e não é resposta vazia. Dizer "não há nada sob
	// este prefixo" aqui seria a mesma troca que já custou caro duas vezes
	// neste arquivo, de trás para frente.
	let responder!: (achado: EnumerationHandle | null) => void;
	enumerou.mockImplementation(
		() => new Promise<EnumerationHandle | null>((resolve) => (responder = resolve)),
	);
	await montar();
	await pedirApagarPrefixo();

	await screen.findByRole("dialog");
	responder(null);

	await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
	expect(screen.queryByTestId("erro-enumerar")).toBeNull();
	expect(screen.queryByTestId("aviso-apagar")).toBeNull();
	expect(screen.queryByTestId("erro-apagar")).toBeNull();
});

test("um pedido de parada que não chega tem assunto próprio", async () => {
	const user = userEvent.setup();
	enumerou.mockImplementation(() => new Promise<EnumerationHandle | null>(() => {}));
	desistiuDaVarredura.mockRejectedValue({
		kind: "unreachable",
		message: "o pedido de parada não chegou",
	});
	await montar();
	await pedirApagarPrefixo();

	await screen.findByRole("dialog");
	await user.keyboard("{Escape}");

	const aviso = await screen.findByTestId("erro-apagar");
	expect(aviso).toHaveTextContent("o pedido de parada não chegou");
	// A VARREDURA continua correndo — não a exclusão, que nunca começou.
	// Nenhuma chave está em risco, e dizer "não deu para apagar as chaves"
	// descreveria outra coisa.
	expect(aviso).toHaveTextContent(/varredura continua correndo/i);
	expect(aviso).not.toHaveTextContent(/não deu para apagar as chaves/i);
});

// ------------------------------------------- a leitura da cópia na janela

test("a janela mostra a leitura da cópia enquanto nenhuma chave saiu", async () => {
	const user = userEvent.setup();
	let entregarLeitura!: (lidas: number) => void;
	vi.mocked(onBulkBackupProgress).mockImplementation(async (handler) => {
		entregarLeitura = handler;
		return () => {};
	});
	apagou.mockImplementation(() => new Promise<WriteOutcome>(() => {}));
	await montar();
	await pedirApagarPrefixo();

	const dialogo = await screen.findByRole("dialog");
	await user.click(within(dialogo).getByRole("button", { name: "Apagar 1.200 chaves" }));

	// Antes de qualquer retrato, é o que a janela sempre mostrou.
	expect(screen.getByTestId("andamento-apagar")).toHaveTextContent("0 chaves apagadas");

	entregarLeitura(400);
	expect(await screen.findByTestId("andamento-apagar")).toHaveTextContent(
		"Lendo a cópia: 400 de 1.200 chaves",
	);
});

// ------------------------------------------- a máquina de estados é total

test("um desfecho que esta tela não conhece fecha o diálogo em vez de travá-lo", async () => {
	const user = userEvent.setup();
	// `applied` é a forma de uma escrita de chave única; a exclusão em
	// massa nunca a devolve hoje. Se um dia devolver, o diálogo não pode
	// ficar na tela para sempre com o botão desabilitado e nada a clicar.
	apagou.mockResolvedValue({ outcome: "applied", entry_id: 1 });
	await montar();
	await pedirApagarPrefixo();

	const dialogo = await screen.findByRole("dialog");
	await user.click(within(dialogo).getByRole("button", { name: "Apagar 1.200 chaves" }));

	await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
});

test("o desfecho apaga a frase da desistência que não vale mais", async () => {
	const user = userEvent.setup();
	let concluir!: (saida: WriteOutcome) => void;
	apagou.mockImplementation(
		() => new Promise<WriteOutcome>((resolve) => (concluir = resolve)),
	);
	desistiu.mockRejectedValue({
		kind: "unreachable",
		message: "o pedido de parada não chegou",
	});
	await montar();
	await pedirApagarPrefixo();

	const dialogo = await screen.findByRole("dialog");
	await user.click(within(dialogo).getByRole("button", { name: "Apagar 1.200 chaves" }));
	await user.keyboard("{Escape}");
	expect(await screen.findByTestId("erro-apagar")).toHaveTextContent(/continua correndo/i);

	// A exclusão termina normalmente: o que não chegou foi o pedido de
	// parada, e nada obriga o apagar a falhar junto.
	concluir({
		outcome: "deleted",
		entry_id: 9,
		deleted: 1_200,
		attempted: 1_200,
		enumerated: 1_200,
	});

	expect(await screen.findByTestId("aviso-apagar")).toHaveTextContent("1.200");
	// A frase descrevia uma exclusão em curso, e agora o modal que a
	// escondia saiu. Deixá-la faria a tela afirmar, lado a lado, que as
	// chaves saíram e que ainda estão saindo.
	expect(screen.queryByTestId("erro-apagar")).toBeNull();
});
