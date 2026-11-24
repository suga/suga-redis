import { beforeEach, expect, test, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
	cancelDelete,
	deletePrefix,
	enumeratePrefix,
	onBulkDeleteProgress,
	onBulkScanProgress,
} from "$lib/api";

const chamado = vi.mocked(invoke);
const escutado = vi.mocked(listen);

beforeEach(() => {
	vi.clearAllMocks();
});

test("enumeratePrefix chama cmd_enumerate_prefix com host, banco e prefixos", async () => {
	const resposta = {
		id: "enum-1",
		count: 3,
		kinds: [["string", 2], ["hash", 1]],
		truncated: false,
		unrestorable: 0,
	};
	chamado.mockResolvedValueOnce(resposta);

	const devolvido = await enumeratePrefix("host-1", 0, ["sessao:", "fila:"]);

	expect(chamado).toHaveBeenCalledWith("cmd_enumerate_prefix", {
		hostId: "host-1",
		db: 0,
		prefixes: ["sessao:", "fila:"],
	});
	expect(devolvido).toEqual(resposta);
});

test("deletePrefix manda o nome digitado quando há um, e null quando não há", async () => {
	chamado.mockResolvedValueOnce({ outcome: "deleted", entry_id: 9, deleted: 3, enumerated: 3 });
	await deletePrefix("enum-1", "inline", "meu-host");

	expect(chamado).toHaveBeenCalledWith("cmd_delete_prefix", {
		enumerationId: "enum-1",
		backup: "inline",
		typedHostName: "meu-host",
	});

	chamado.mockResolvedValueOnce({ outcome: "needs_confirmation", summary: "apaga 3 chaves" });
	await deletePrefix("enum-1", "inline", null);

	expect(chamado).toHaveBeenCalledWith("cmd_delete_prefix", {
		enumerationId: "enum-1",
		backup: "inline",
		typedHostName: null,
	});
});

test("cancelDelete chama cmd_cancel_delete com o id da enumeração", async () => {
	chamado.mockResolvedValueOnce(true);
	const parou = await cancelDelete("enum-1");

	expect(chamado).toHaveBeenCalledWith("cmd_cancel_delete", { enumerationId: "enum-1" });
	expect(parou).toBe(true);
});

test("onBulkScanProgress escuta bulk://scan-progress", async () => {
	const parar = vi.fn();
	escutado.mockResolvedValueOnce(parar);

	const tick = { scanned: 100, total_estimate: 500 };
	const recebido = vi.fn();
	await onBulkScanProgress(recebido);

	expect(escutado).toHaveBeenCalledWith("bulk://scan-progress", expect.any(Function));
	const handler = escutado.mock.calls[0][1];
	handler({ payload: tick } as Parameters<typeof handler>[0]);
	expect(recebido).toHaveBeenCalledWith(tick);
});

test("onBulkDeleteProgress escuta bulk://delete-progress", async () => {
	const parar = vi.fn();
	escutado.mockResolvedValueOnce(parar);

	const recebido = vi.fn();
	await onBulkDeleteProgress(recebido);

	expect(escutado).toHaveBeenCalledWith("bulk://delete-progress", expect.any(Function));
	const handler = escutado.mock.calls[0][1];
	handler({ payload: 42 } as Parameters<typeof handler>[0]);
	expect(recebido).toHaveBeenCalledWith(42);
});
