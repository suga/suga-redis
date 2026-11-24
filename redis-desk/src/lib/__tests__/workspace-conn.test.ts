import { render, screen, waitFor } from "@testing-library/svelte";
import { tick } from "svelte";
import { beforeEach, expect, test, vi } from "vitest";

// `Workspace` monta `KeyTree` de qualquer jeito (coluna da esquerda), então
// o mock precisa cobrir tudo que ELE chama em `onMount`/efeitos também —
// não só o que `Workspace` importa direto. Com nenhum galho aberto por
// padrão, `KeyTree` não chama `treeLevel`/`loadMoreChildren` sozinho, mas o
// import estático ainda precisa resolver para alguma coisa.
vi.mock("$lib/api", () => ({
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
	enumeratePrefix: vi.fn(),
	deletePrefix: vi.fn(),
	cancelDelete: vi.fn(async () => true),
}));

import { onTreeProgress, scanTree } from "$lib/api";
import Workspace from "$lib/Workspace.svelte";
import type { Host, TreeProgress } from "$lib/api";

const varreu = vi.mocked(scanTree);

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
});

test("conexão já sabida como falha não dispara varredura, e mostra o motivo", async () => {
	render(Workspace, {
		props: { host: host(), falhaConexao: "não foi possível alcançar o host" },
	});

	expect(await screen.findByRole("alert")).toHaveTextContent(
		"não foi possível alcançar o host",
	);
	// Nada de "Lendo as chaves em segundo plano" por cima da falha: as duas
	// telas ao mesmo tempo era exatamente a confusão que esta tarefa existe
	// para acabar.
	expect(screen.queryByTestId("lendo")).not.toBeInTheDocument();
	expect(varreu).not.toHaveBeenCalled();
});

test("sem falha conhecida, abre o banco e varre normalmente", async () => {
	render(Workspace, {
		props: { host: host(), falhaConexao: null },
	});

	await waitFor(() => expect(varreu).toHaveBeenCalledWith("h1", 0));
});

test("uma varredura que não alcança o host avisa quem abriu", async () => {
	// Um `Error`, e não o objeto cru do Tauri: o mock de `errorMessage`
	// neste arquivo é `String(err)`, que de um objeto devolveria
	// "[object Object]" e esconderia o que está sendo verificado.
	varreu.mockRejectedValueOnce(new Error("não foi possível conectar ao host"));
	const avisou = vi.fn();

	render(Workspace, {
		props: { host: host(), falhaConexao: null, onFalhaDeConexao: avisou },
	});

	// A casca repassa isto à coluna de hosts, para a linha parar de dizer
	// "conectado" enquanto esta tela mostra o contrário.
	await waitFor(() =>
		expect(avisou).toHaveBeenCalledWith(
			expect.stringContaining("não foi possível conectar ao host"),
		),
	);
});

test("um retrato de outro host não entra na árvore deste", async () => {
	let entregar: ((retrato: TreeProgress) => void) | null = null;
	vi.mocked(onTreeProgress).mockImplementation(async (handler) => {
		entregar = handler;
		return () => {};
	});

	render(Workspace, { props: { host: host(), falhaConexao: null } });
	await waitFor(() => expect(entregar).not.toBeNull());

	function retrato(partes: Partial<TreeProgress>): TreeProgress {
		return {
			host_id: "h1",
			db: 0,
			scanned: 1,
			total_estimate: 1,
			root: [],
			done: false,
			...partes,
		};
	}

	const galho = {
		label: "producao",
		full_prefix: "producao:",
		count: 9,
		leaf_key: null,
	};

	// A varredura do host A segue correndo no Rust depois do `disconnect`
	// e continua emitindo. O host B, aberto em seguida, não pode receber as
	// chaves dela.
	entregar!(retrato({ host_id: "outro-host", root: [galho] }));
	await tick();
	expect(screen.queryByText("producao")).not.toBeInTheDocument();

	// Nem o mesmo host em outro banco.
	entregar!(retrato({ db: 5, root: [galho] }));
	await tick();
	expect(screen.queryByText("producao")).not.toBeInTheDocument();

	// E o retrato do par aberto entra normalmente.
	entregar!(retrato({ root: [galho] }));
	expect(await screen.findByText("producao")).toBeInTheDocument();
});
