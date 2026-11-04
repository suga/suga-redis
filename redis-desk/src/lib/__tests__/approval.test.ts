import { render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";

vi.mock("$lib/api", async (importOriginal) => {
	const real = await importOriginal<typeof import("$lib/api")>();
	return { ...real, resolveApproval: vi.fn(), denyApproval: vi.fn() };
});

import { resolveApproval } from "$lib/api";
import ApprovalDialog from "$lib/ApprovalDialog.svelte";
import type { ApprovalRequest } from "$lib/api";

const liberou = vi.mocked(resolveApproval);

function pedido(partes: Partial<ApprovalRequest> = {}): ApprovalRequest {
	return {
		id: "pedido-1",
		host_id: "h1",
		host_name: "Redis pagamentos",
		env: "prod",
		db: 0,
		key: "sessao:alpha",
		summary: "Definir o valor de sessao:alpha",
		op: { op: "set_string", value: { text: "novo", base64: null, truncated: false } },
		asked_at: 1_762_000_000_000,
		...partes,
	};
}

function montar(req = pedido()) {
	return render(ApprovalDialog, {
		props: { request: req, onSettled: vi.fn() },
	});
}

beforeEach(() => {
	vi.clearAllMocks();
});

test("pedido do Claude aparece com host, chave e o que vai mudar", () => {
	montar();

	const dialogo = screen.getByRole("dialog");
	expect(dialogo).toHaveTextContent("Redis pagamentos");
	expect(dialogo).toHaveTextContent("sessao:alpha");
	expect(dialogo).toHaveTextContent("Definir o valor de sessao:alpha");
	// E deixa claro que quem pediu não foi a pessoa.
	expect(dialogo).toHaveTextContent("Claude");
});

test("botão liberar fica desligado até o nome do host estar digitado certo", async () => {
	const user = userEvent.setup();
	montar();

	const liberar = screen.getByRole("button", { name: "Liberar a escrita" });
	expect(liberar).toBeDisabled();

	const campo = screen.getByLabelText("Digite o nome do host para liberar");
	await user.type(campo, "Redis pagament");
	expect(liberar).toBeDisabled();

	await user.type(campo, "os");
	expect(liberar).toBeEnabled();

	await user.click(liberar);
	expect(liberou).toHaveBeenCalledWith("pedido-1", "Redis pagamentos");
});

test("nome digitado errado mostra o que se espera e não libera", async () => {
	const user = userEvent.setup();
	montar();

	await user.type(
		screen.getByLabelText("Digite o nome do host para liberar"),
		"redis pagamentos",
	);

	// Caixa diferente não serve, e a tela diz exatamente o que falta.
	const aviso = await screen.findByRole("status");
	expect(aviso).toHaveTextContent("Redis pagamentos");
	expect(screen.getByRole("button", { name: "Liberar a escrita" })).toBeDisabled();
	expect(liberou).not.toHaveBeenCalled();
});
