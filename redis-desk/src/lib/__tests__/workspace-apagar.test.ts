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
import { render, screen, waitFor, within } from "@testing-library/svelte";
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
		scanTree: vi.fn(async () => undefined),
		rescan: vi.fn(),
		scanStatus: vi.fn(async () => null),
		treeLevel: vi.fn(async () => null),
		loadMoreChildren: vi.fn(),
		writeValue: vi.fn(),
		enumeratePrefix: vi.fn(),
		deletePrefix: vi.fn(),
		cancelDelete: vi.fn(async () => true),
	};
});

import {
	cancelDelete,
	deletePrefix,
	enumeratePrefix,
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
		enumerated: 1_200,
	}));
	desistiu.mockImplementation(async () => true);
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
		"Apagar chave…",
	]);
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
	await user.click(within(menu).getByRole("menuitem", { name: "Apagar chave…" }));

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

	// O diálogo não tem trava própria de propósito (uma recusa por nome
	// precisa deixá-lo usável), então a trava é daqui: sem ela, a segunda
	// chamada chega com a mesma enumeração e volta `UnknownEnumeration`.
	await user.click(botao);
	await user.click(botao);

	expect(apagou).toHaveBeenCalledTimes(1);
	liberar({ outcome: "deleted", entry_id: 9, deleted: 1_200, enumerated: 1_200 });
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

	// A enumeração foi consumida antes da cópia ser lida, então reabrir
	// custa uma varredura nova — e a segunda confirmação vai no id novo.
	expect(enumerou).toHaveBeenCalledTimes(2);

	await user.click(within(reaberto).getByRole("button", { name: "Apagar 1.200 chaves" }));
	expect(apagou).toHaveBeenLastCalledWith("enum-1", "file", null);
});

test("recusada também em arquivo, só restam apagar sem cópia e cancelar", async () => {
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
