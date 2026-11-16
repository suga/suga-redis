/**
 * A área de trabalho e a árvore em outro idioma.
 *
 * Vive separado de `i18n-tela.test.ts` porque o `Workspace` precisa do
 * módulo `$lib/api` inteiro dublado, e um `vi.mock` vale para o arquivo
 * todo — misturar derrubaria os componentes que falam com a API de verdade.
 */
import { render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

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

import type { Host } from "$lib/api";
import { definirIdioma } from "$lib/i18n.svelte";
import Workspace from "$lib/Workspace.svelte";

beforeEach(() => {
	vi.clearAllMocks();
});

afterEach(() => {
	definirIdioma("pt-BR");
});

function host(): Host {
	return {
		id: "h1",
		name: "Redis de testes",
		addr: "127.0.0.1",
		port: 6379,
		db: 0,
		username: null,
		tls: false,
		env: "dev",
	};
}

test("a área de trabalho fala inglês", async () => {
	definirIdioma("en");

	render(Workspace, { props: { host: host(), falhaConexao: null } });

	expect(await screen.findByRole("button", { name: "Rescan" })).toBeInTheDocument();
	expect(screen.getByRole("button", { name: "New key" })).toBeInTheDocument();
	expect(screen.getByRole("button", { name: "History" })).toBeInTheDocument();
	expect(screen.getByPlaceholderText("Filter what is open")).toBeInTheDocument();
});

test("a árvore vazia fala espanhol", async () => {
	definirIdioma("es");

	render(Workspace, { props: { host: host(), falhaConexao: null } });

	expect(await screen.findByText("No hay claves en esta base.")).toBeInTheDocument();
});
