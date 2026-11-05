import { beforeEach, expect, test, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { connect, disconnect, logs } from "$lib/api";

const chamado = vi.mocked(invoke);

beforeEach(() => {
	vi.clearAllMocks();
});

test("fala com os comandos certos", async () => {
	chamado.mockResolvedValueOnce([]);
	await logs();
	expect(chamado).toHaveBeenCalledWith("cmd_logs");

	chamado.mockResolvedValueOnce(12);
	await connect("host-1");
	expect(chamado).toHaveBeenCalledWith("cmd_connect", { hostId: "host-1" });

	chamado.mockResolvedValueOnce(undefined);
	await disconnect("host-1");
	expect(chamado).toHaveBeenCalledWith("cmd_disconnect", { hostId: "host-1" });
});
