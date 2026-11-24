import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { tick } from "svelte";
import { beforeEach, expect, test, vi } from "vitest";

// Mesmo mínimo de mock que `workspace-conn.test.ts`: `Workspace` monta
// `KeyTree` de qualquer jeito, então o mock cobre tudo que ele chama também.
vi.mock("$lib/api", async (importOriginal) => ({
	// Do módulo real, e não um número repetido aqui: um mock literal que
	// esquece um export novo mata o primeiro teste que montar o diálogo,
	// com uma mensagem que não tem nada a ver com o que ele testava.
	MAX_BACKUP_BYTES: (await importOriginal<typeof import("$lib/api")>()).MAX_BACKUP_BYTES,
	errorMessage: (err: unknown) => String(err),
	textBytes: (text: string) => ({ text, base64: null, truncated: false }),
	isApiError: (err: unknown) =>
		typeof err === "object" && err !== null && "kind" in err && "message" in err,
	listDbs: vi.fn(async () => []),
	onRescanProgress: vi.fn(async () => () => {}),
	onTreeProgress: vi.fn(async () => () => {}),
	scanTree: vi.fn(async () => undefined),
	rescan: vi.fn(),
	scanStatus: vi.fn(async () => null),
	writeValue: vi.fn(),
	treeLevel: vi.fn(async () => null),
	loadMoreChildren: vi.fn(),
	// O `Workspace` passou a assinar os dois andamentos da exclusão em
	// massa na montagem; sem eles aqui, ele nem monta.
	onBulkScanProgress: vi.fn(async () => () => {}),
	onBulkDeleteProgress: vi.fn(async () => () => {}),
	onBulkBackupProgress: vi.fn(async () => () => {}),
	enumeratePrefix: vi.fn(),
	deletePrefix: vi.fn(),
	cancelDelete: vi.fn(async () => true),
	cancelEnumeration: vi.fn(async () => true),
}));

import { listDbs, rescan, scanStatus, scanTree } from "$lib/api";
import Workspace from "$lib/Workspace.svelte";
import type { Host } from "$lib/api";

const bancos = vi.mocked(listDbs);
const indice = vi.mocked(scanStatus);
const varredura = vi.mocked(rescan);
const arvore = vi.mocked(scanTree);

/** Uma promessa controlável de fora — para simular uma resposta que demora. */
function promessaControlavel<T>() {
	let resolve!: (valor: T) => void;
	let reject!: (motivo: unknown) => void;
	const promessa = new Promise<T>((res, rej) => {
		resolve = res;
		reject = rej;
	});
	return { promessa, resolve, reject };
}

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

beforeEach(() => {
	vi.clearAllMocks();
	bancos.mockImplementation(async () => []);
	indice.mockImplementation(async () => null);
	arvore.mockImplementation(async () => undefined);
});

test("mostra o erro quando a lista de bancos falha, em vez de um seletor pelado", async () => {
	bancos.mockImplementation(async () => {
		throw "não foi possível alcançar o host";
	});

	render(Workspace, { props: { host: host(), falhaConexao: null } });

	expect(await screen.findByTestId("erro-bancos")).toHaveTextContent(
		"não foi possível alcançar o host",
	);
});

test("mostra o erro quando o relatório de índice falha", async () => {
	indice.mockImplementation(async () => {
		throw "não foi possível perguntar ao servidor";
	});

	render(Workspace, { props: { host: host(), falhaConexao: null } });

	expect(await screen.findByTestId("erro-indice")).toHaveTextContent(
		"não foi possível perguntar ao servidor",
	);
});

test("distingue banco vazio de falha ao perguntar", async () => {
	// Banco vazio: o servidor respondeu, e a resposta é "nunca varrido" — não
	// há erro nenhum para mostrar.
	indice.mockImplementation(async () => null);
	const { unmount } = render(Workspace, {
		props: { host: host(), falhaConexao: null },
	});

	await waitFor(() => expect(indice).toHaveBeenCalled());
	expect(screen.getByText(/nunca varrido/)).toBeInTheDocument();
	expect(screen.queryByTestId("erro-indice")).not.toBeInTheDocument();
	unmount();

	// Falha ao perguntar: a mesma pergunta não pôde ser respondida — e isso
	// não pode virar, silenciosamente, o mesmo "nunca varrido" de cima.
	indice.mockImplementation(async () => {
		throw "não foi possível perguntar ao servidor";
	});
	render(Workspace, { props: { host: host(), falhaConexao: null } });

	expect(await screen.findByTestId("erro-indice")).toHaveTextContent(
		"não foi possível perguntar ao servidor",
	);
	expect(screen.queryByText(/nunca varrido/)).not.toBeInTheDocument();
});

test("Varrer de novo depois de uma falha de índice não deixa o aviso antigo sobreviver", async () => {
	indice.mockImplementation(async () => {
		throw "não foi possível perguntar ao servidor";
	});
	varredura.mockResolvedValue({
		host_id: "h1",
		db: 0,
		keys: 7,
		finished_at: Date.now(),
		duration_ms: 5,
	});

	render(Workspace, { props: { host: host(), falhaConexao: null } });

	// A falha aparece primeiro, como nos outros testes.
	await screen.findByTestId("erro-indice");

	// A pessoa clica no botão bem ao lado do aviso — a ação corretiva óbvia.
	await fireEvent.click(screen.getByRole("button", { name: "Varrer de novo" }));

	// O `rescan` deu certo: a linha já diz que o índice está em dia, e o
	// aviso de falha de baixo da linha não pode continuar ali, contradizendo
	// o que a própria linha agora afirma.
	await waitFor(() =>
		expect(screen.queryByTestId("erro-indice")).not.toBeInTheDocument(),
	);
	expect(
		screen.queryByText(/não foi possível perguntar/),
	).not.toBeInTheDocument();
});

test("mostra que ainda está perguntando ao índice, em vez de parecer nunca varrido", async () => {
	const { promessa, resolve } = promessaControlavel<null>();
	indice.mockImplementation(() => promessa);

	render(Workspace, { props: { host: host(), falhaConexao: null } });

	// Enquanto a pergunta não voltou — o caso do host lento do incidente,
	// 5679 ms só para um PING — a tela não pode parecer uma resposta já
	// dada (nem "nunca varrido", nem um erro que ainda não aconteceu).
	await waitFor(() => expect(indice).toHaveBeenCalled());
	expect(screen.getByText(/perguntando ao servidor/)).toBeInTheDocument();
	expect(screen.queryByText(/nunca varrido/)).not.toBeInTheDocument();
	expect(screen.queryByTestId("erro-indice")).not.toBeInTheDocument();

	// A resposta chega: aí sim vira "nunca varrido", legitimamente.
	resolve(null);
	await waitFor(() =>
		expect(screen.getByText(/nunca varrido/)).toBeInTheDocument(),
	);
});

test("troca rápida de banco: uma resposta atrasada da pergunta antiga não sobrescreve a mais nova", async () => {
	bancos.mockImplementation(async () => [
		[0, 10],
		[1, 20],
	]);
	const chamadas: Array<ReturnType<typeof promessaControlavel<null>>> = [];
	indice.mockImplementation(() => {
		const controlavel = promessaControlavel<null>();
		chamadas.push(controlavel);
		return controlavel.promessa;
	});

	render(Workspace, { props: { host: host(), falhaConexao: null } });

	// A primeira pergunta (banco 0, o `db` inicial) parte sozinha.
	await waitFor(() => expect(chamadas).toHaveLength(1));

	const seletor = screen.getByRole("combobox") as HTMLSelectElement;
	await waitFor(() => expect(seletor.querySelectorAll("option")).toHaveLength(2));

	// Troca para o banco 1 antes da primeira pergunta voltar — dispara a
	// segunda, mais nova.
	await fireEvent.change(seletor, { target: { value: "1" } });
	await waitFor(() => expect(chamadas).toHaveLength(2));

	// A pergunta nova (banco 1) responde primeiro, legitimamente vazia.
	chamadas[1].resolve(null);
	await waitFor(() =>
		expect(screen.getByText(/nunca varrido/)).toBeInTheDocument(),
	);

	// A pergunta antiga (banco 0) só responde depois, e falhando — mas o
	// banco aberto já não é mais aquele, e essa resposta atrasada não pode
	// sobrescrever o que o banco atual, já respondido, mostra.
	chamadas[0].reject("não foi possível perguntar ao servidor");
	await new Promise((resolver) => setTimeout(resolver, 0));

	expect(screen.getByText(/nunca varrido/)).toBeInTheDocument();
	expect(screen.queryByTestId("erro-indice")).not.toBeInTheDocument();
});

test("Varrer de novo: trocar de banco antes do rescan voltar não deixa o resultado antigo aterrissar", async () => {
	bancos.mockImplementation(async () => [
		[0, 10],
		[1, 20],
	]);
	// O índice já respondeu, legitimamente vazio, para os dois bancos —
	// o que está em jogo aqui é só a resposta do `rescan`, não a dele.
	indice.mockImplementation(async () => null);

	const { promessa, resolve } = promessaControlavel<{
		host_id: string;
		db: number;
		keys: number;
		finished_at: number;
		duration_ms: number;
	}>();
	varredura.mockImplementation(() => promessa);

	render(Workspace, { props: { host: host(), falhaConexao: null } });
	await waitFor(() => expect(screen.getByText(/nunca varrido/)).toBeInTheDocument());

	// Clica em "Varrer de novo" no banco 0 (o aberto) — o `rescan` fica
	// pendurado, controlado por este teste.
	await fireEvent.click(screen.getByRole("button", { name: "Varrer de novo" }));
	await waitFor(() => expect(varredura).toHaveBeenCalledTimes(1));

	// Uma pessoa de verdade não consegue mais fazer esta troca por aqui — o
	// seletor desliga junto com `varrendo` — mas o teste dispara o evento
	// direto no elemento mesmo assim, como defesa a mais: se o valor mudar
	// por qualquer outro caminho, `geracaoIndice` ainda tem que impedir que
	// a resposta do banco abandonado aterrisse em `relatorio`/`erro`.
	const seletor = screen.getByRole("combobox") as HTMLSelectElement;
	await waitFor(() => expect(seletor.querySelectorAll("option")).toHaveLength(2));
	expect(seletor).toBeDisabled();
	await fireEvent.change(seletor, { target: { value: "1" } });
	// `progresso` e `varrendo` não carregam geração nenhuma — não há como
	// filtrá-los pelo banco, por isso a linha segue dizendo "Varrendo…" até
	// o `rescan` abandonado liberar o `finally`. É exatamente por isso que
	// a correção de verdade é desligar o seletor, não alargar a guarda.
	await waitFor(() => expect(screen.getByText(/Varrendo/)).toBeInTheDocument());

	// Só agora o `rescan` do banco 0, já abandonado, responde — com um
	// relatório que não é mais sobre o banco aberto.
	resolve({
		host_id: "h1",
		db: 0,
		keys: 999,
		finished_at: Date.now(),
		duration_ms: 1,
	});
	await waitFor(() => expect(screen.getByText(/nunca varrido/)).toBeInTheDocument());

	// O banco 1 continua mostrando a resposta que já tinha — nada do
	// `rescan` atrasado do banco 0 pode ter aterrissado em cima dela.
	expect(screen.getByText(/nunca varrido/)).toBeInTheDocument();
	expect(screen.queryByTestId("erro-indice")).not.toBeInTheDocument();
});

test("o seletor de banco desliga durante a varredura manual e volta a ligar depois", async () => {
	bancos.mockImplementation(async () => [
		[0, 10],
		[1, 20],
	]);
	indice.mockImplementation(async () => null);

	const { promessa, resolve } = promessaControlavel<{
		host_id: string;
		db: number;
		keys: number;
		finished_at: number;
		duration_ms: number;
	}>();
	varredura.mockImplementation(() => promessa);

	render(Workspace, { props: { host: host(), falhaConexao: null } });
	await waitFor(() => expect(screen.getByText(/nunca varrido/)).toBeInTheDocument());

	const seletor = screen.getByRole("combobox") as HTMLSelectElement;
	expect(seletor).toBeEnabled();

	await fireEvent.click(screen.getByRole("button", { name: "Varrer de novo" }));

	// Com o `rescan` ainda em voo, trocar de banco por aqui não é mais
	// possível — a classe inteira de resposta atrasada sem geração
	// (`progresso`, `varrendo`) fica fechada por não haver como abrir o
	// seletor para chegar nela.
	await waitFor(() => expect(seletor).toBeDisabled());

	resolve({
		host_id: "h1",
		db: 0,
		keys: 3,
		finished_at: Date.now(),
		duration_ms: 1,
	});

	await waitFor(() => expect(seletor).toBeEnabled());
});

test("o seletor de banco continua utilizável durante a varredura automática", async () => {
	bancos.mockImplementation(async () => [
		[0, 10],
		[1, 20],
	]);
	const emVoo = promessaControlavel<undefined>();
	arvore.mockImplementation(() => emVoo.promessa);

	render(Workspace, { props: { host: host(), falhaConexao: null } });

	// A varredura automática é a primeira coisa que acontece num banco
	// recém-aberto e, num banco de milhões de chaves, demora. Desligar o
	// seletor aqui congelaria o controle por quase um minuto sem ninguém
	// ter pedido nada — diferente da varredura manual, em que a pessoa
	// clicou sabendo que ia esperar. A corrida fica por conta da geração.
	const seletor = await screen.findByRole("combobox");
	await waitFor(() => expect(screen.getByTestId("lendo")).toBeInTheDocument());
	expect(seletor).toBeEnabled();

	emVoo.resolve(undefined);
	await waitFor(() => expect(seletor).toBeEnabled());
});

test("trocar de banco no meio da varredura descarta a falha da anterior", async () => {
	bancos.mockImplementation(async () => [
		[0, 10],
		[1, 20],
	]);
	const banco0 = promessaControlavel<undefined>();
	const banco1 = promessaControlavel<undefined>();
	arvore.mockImplementationOnce(() => banco0.promessa);
	arvore.mockImplementationOnce(() => banco1.promessa);

	render(Workspace, { props: { host: host(), falhaConexao: null } });
	const seletor = await screen.findByRole("combobox");
	await waitFor(() => expect(arvore).toHaveBeenCalledWith("h1", 0));

	// Agora que o seletor segue ligado, esta é a corrida de verdade: trocar
	// para o banco 1 com a varredura do banco 0 ainda em voo.
	await fireEvent.change(seletor, { target: { value: "1" } });
	await waitFor(() => expect(arvore).toHaveBeenCalledWith("h1", 1));

	banco0.reject(new Error("a varredura do banco 0 falhou"));
	await tick();
	await tick();

	// A falha do banco 0 não pode aparecer na tela do banco 1.
	expect(screen.queryByRole("alert")).not.toBeInTheDocument();
	banco1.resolve(undefined);
});

test("a falha de uma varredura antiga não aterrissa sobre o estado novo", async () => {
	const emVoo = promessaControlavel<undefined>();
	arvore.mockImplementation(() => emVoo.promessa);

	const { rerender } = render(Workspace, {
		props: { host: host(), falhaConexao: null },
	});
	await waitFor(() => expect(arvore).toHaveBeenCalledTimes(1));

	// Uma nova tentativa de conexão ao MESMO host falhou enquanto a
	// varredura anterior ainda corria: a tela já está mostrando esse
	// motivo.
	await rerender({ falhaConexao: "não foi possível alcançar o host" });
	expect(await screen.findByRole("alert")).toHaveTextContent(
		"não foi possível alcançar o host",
	);

	emVoo.reject(new Error("erro da varredura que já não interessa"));
	await tick();
	await tick();

	// A resposta atrasada não pode trocar a mensagem que está na tela: era
	// o mesmo defeito já corrigido para a varredura manual e para a
	// pergunta de índice, deixado aberto no caminho automático.
	expect(screen.getByRole("alert")).toHaveTextContent("não foi possível alcançar o host");
	expect(screen.queryByTestId("lendo")).not.toBeInTheDocument();
});

test("Varrer de novo relê a árvore, não só o índice", async () => {
	// O botão vive ao lado de "Índice de busca:" e só reconstruía o índice
	// de valores. Quem clica nele quer ver as ATUALIZAÇÕES — chaves novas,
	// chaves que sumiram — e isso exige revarrer a árvore.
	varredura.mockResolvedValue({ keys: 0, bytes: 0, took_ms: 1 } as never);

	render(Workspace, { props: { host: host(), falhaConexao: null } });
	await waitFor(() => expect(arvore).toHaveBeenCalledTimes(1));

	await fireEvent.click(screen.getByRole("button", { name: "Varrer de novo" }));

	await waitFor(() => expect(arvore).toHaveBeenCalledTimes(2));
	expect(varredura).toHaveBeenCalledTimes(1);
});

test("Varrer de novo mostra a barra de progresso da varredura", async () => {
	// A mesma barra da abertura do banco: é ela que diz quanto falta.
	const rescanEmVoo = promessaControlavel<unknown>();
	varredura.mockImplementation(() => rescanEmVoo.promessa as never);
	const arvoreEmVoo = promessaControlavel<void>();

	render(Workspace, { props: { host: host(), falhaConexao: null } });
	await waitFor(() => expect(arvore).toHaveBeenCalledTimes(1));

	arvore.mockImplementation(() => arvoreEmVoo.promessa);
	await fireEvent.click(screen.getByRole("button", { name: "Varrer de novo" }));

	expect(await screen.findByTestId("lendo")).toBeInTheDocument();

	arvoreEmVoo.resolve();
	rescanEmVoo.resolve({ keys: 0, bytes: 0, took_ms: 1 });
});
