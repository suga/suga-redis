import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { beforeEach, expect, test, vi } from "vitest";

// Mesmo mínimo de mock que `workspace-conn.test.ts`: `Workspace` monta
// `KeyTree` de qualquer jeito, então o mock cobre tudo que ele chama também.
vi.mock("$lib/api", () => ({
	errorMessage: (err: unknown) => String(err),
	textBytes: (text: string) => ({ text, base64: null, truncated: false }),
	listDbs: vi.fn(async () => []),
	onRescanProgress: vi.fn(async () => () => {}),
	onTreeProgress: vi.fn(async () => () => {}),
	scanTree: vi.fn(async () => undefined),
	rescan: vi.fn(),
	scanStatus: vi.fn(async () => null),
	writeValue: vi.fn(),
	treeLevel: vi.fn(async () => null),
	loadMoreChildren: vi.fn(),
}));

import { listDbs, rescan, scanStatus } from "$lib/api";
import Workspace from "$lib/Workspace.svelte";
import type { Host } from "$lib/api";

const bancos = vi.mocked(listDbs);
const indice = vi.mocked(scanStatus);
const varredura = vi.mocked(rescan);

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
