import { render, screen, waitFor } from "@testing-library/svelte";
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

import { listDbs, scanStatus } from "$lib/api";
import Workspace from "$lib/Workspace.svelte";
import type { Host } from "$lib/api";

const bancos = vi.mocked(listDbs);
const indice = vi.mocked(scanStatus);

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
