import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";

vi.mock("$lib/api", async (importOriginal) => {
	const real = await importOriginal<typeof import("$lib/api")>();
	return { ...real, writeValue: vi.fn() };
});

import { writeValue } from "$lib/api";
import KeyEditor from "$lib/KeyEditor.svelte";
import NewKeyDialog from "$lib/NewKeyDialog.svelte";
import type { Bytes, ValueView } from "$lib/api";

const escrito = vi.mocked(writeValue);

function texto(valor: string): Bytes {
	return { text: valor, base64: null, truncated: false };
}

function view(partes: Partial<ValueView> = {}): ValueView {
	return {
		key: "sessao:alpha",
		kind: "string",
		ttl_ms: null,
		size_bytes: 5,
		value: { kind: "string", data: texto("antes") },
		total_elements: 1,
		truncated: false,
		...partes,
	};
}

function montar(v: ValueView = view()) {
	return render(KeyEditor, {
		props: { hostId: "h1", db: 0, view: v, onWritten: vi.fn() },
	});
}

/** O backend em produção: nada é escrito até alguém confirmar. */
function exigeConfirmacao(summary: string) {
	escrito.mockResolvedValueOnce({ outcome: "needs_confirmation", summary });
}

function aplicado(entryId = 1) {
	escrito.mockResolvedValueOnce({ outcome: "applied", entry_id: entryId });
}

beforeEach(() => {
	vi.clearAllMocks();
});

test("salvar string em prod abre o confirmar com o resumo da mudança", async () => {
	const user = userEvent.setup();
	exigeConfirmacao("Definir o valor de sessao:alpha");
	montar();

	await user.clear(screen.getByLabelText("Valor"));
	await user.type(screen.getByLabelText("Valor"), "depois");
	await user.click(screen.getByRole("button", { name: "Salvar" }));

	const dialogo = await screen.findByRole("dialog");
	expect(dialogo).toHaveTextContent("Definir o valor de sessao:alpha");
	expect(escrito).toHaveBeenCalledTimes(1);
});

test("confirmar reenvia write_value com confirmed true", async () => {
	const user = userEvent.setup();
	exigeConfirmacao("Definir o valor de sessao:alpha");
	aplicado();
	montar();

	await user.clear(screen.getByLabelText("Valor"));
	await user.type(screen.getByLabelText("Valor"), "depois");
	await user.click(screen.getByRole("button", { name: "Salvar" }));
	await user.click(await screen.findByRole("button", { name: "Confirmar" }));

	expect(escrito).toHaveBeenCalledTimes(2);
	const [, segunda] = escrito.mock.calls;
	expect(segunda).toEqual([
		"h1",
		0,
		"sessao:alpha",
		{ op: "set_string", value: texto("depois") },
		true,
	]);
	await waitFor(() =>
		expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
	);
});

test("cancelar não chama write_value de novo e mantém o texto editado na tela", async () => {
	const user = userEvent.setup();
	exigeConfirmacao("Definir o valor de sessao:alpha");
	montar();

	await user.clear(screen.getByLabelText("Valor"));
	await user.type(screen.getByLabelText("Valor"), "depois");
	await user.click(screen.getByRole("button", { name: "Salvar" }));
	await user.click(await screen.findByRole("button", { name: "Cancelar" }));

	expect(escrito).toHaveBeenCalledTimes(1);
	expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
	// O trabalho da pessoa continua na tela, pronto para ela decidir de novo.
	expect(screen.getByLabelText("Valor")).toHaveValue("depois");
});

test("salvar em dev não abre confirmar", async () => {
	const user = userEvent.setup();
	aplicado();
	montar();

	await user.clear(screen.getByLabelText("Valor"));
	await user.type(screen.getByLabelText("Valor"), "depois");
	await user.click(screen.getByRole("button", { name: "Salvar" }));

	await waitFor(() => expect(escrito).toHaveBeenCalledTimes(1));
	expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

test("editar campo de hash manda SetHashField só do campo mexido", async () => {
	const user = userEvent.setup();
	aplicado();
	montar(
		view({
			key: "cliente:7",
			kind: "hash",
			value: {
				kind: "hash",
				data: [
					["email", texto("ana@empresa.com")],
					["nome", texto("Ana")],
				],
			},
			total_elements: 2,
		}),
	);

	await user.clear(screen.getByLabelText("Valor do campo nome"));
	await user.type(screen.getByLabelText("Valor do campo nome"), "Marco");
	await user.click(screen.getByRole("button", { name: "Salvar campo nome" }));

	await waitFor(() => expect(escrito).toHaveBeenCalledTimes(1));
	expect(escrito).toHaveBeenCalledWith(
		"h1",
		0,
		"cliente:7",
		{ op: "set_hash_field", field: "nome", value: texto("Marco") },
		false,
	);
});

test("remover item de lista manda RemoveListItem com o índice certo", async () => {
	const user = userEvent.setup();
	aplicado();
	montar(
		view({
			key: "fila:pedidos",
			kind: "list",
			value: { kind: "list", data: [texto("um"), texto("dois"), texto("tres")] },
			total_elements: 3,
		}),
	);

	await user.click(screen.getByRole("button", { name: "Remover item 1" }));

	await waitFor(() => expect(escrito).toHaveBeenCalledTimes(1));
	expect(escrito).toHaveBeenCalledWith(
		"h1",
		0,
		"fila:pedidos",
		{ op: "remove_list_item", index: 1 },
		false,
	);
});

test("criar chave nova pede tipo e nome e recusa nome vazio", async () => {
	const user = userEvent.setup();
	const onCreate = vi.fn();
	render(NewKeyDialog, { props: { onCreate, onCancel: vi.fn() } });

	// Nome vazio: não sai nada.
	await user.click(screen.getByRole("button", { name: "Criar" }));
	expect(await screen.findByRole("alert")).toHaveTextContent(
		"Informe o nome da chave.",
	);
	expect(onCreate).not.toHaveBeenCalled();

	await user.type(screen.getByLabelText("Nome da chave"), "sessao:nova");
	await user.selectOptions(screen.getByLabelText("Tipo"), "hash");
	await user.click(screen.getByRole("button", { name: "Criar" }));

	expect(onCreate).toHaveBeenCalledWith({ key: "sessao:nova", kind: "hash" });
});

test("erro de conexão ao salvar mostra o nome do host e não limpa a edição", async () => {
	const user = userEvent.setup();
	escrito.mockRejectedValueOnce({
		kind: "unreachable",
		message: 'não foi possível conectar ao host "Redis pagamentos": timed out',
	});
	montar();

	await user.clear(screen.getByLabelText("Valor"));
	await user.type(screen.getByLabelText("Valor"), "depois");
	await user.click(screen.getByRole("button", { name: "Salvar" }));

	expect(await screen.findByRole("alert")).toHaveTextContent("Redis pagamentos");
	// A edição sobrevive à queda: é o único lugar onde ela existe.
	expect(screen.getByLabelText("Valor")).toHaveValue("depois");
});
