/**
 * A frase que descreve uma escrita.
 *
 * É o texto que a pessoa lê antes de autorizar uma mudança em produção, e
 * o que o histórico mostra depois. Nascia em português no Rust; agora é
 * montada na tela, no idioma de quem está lendo.
 */
import { afterEach, describe, expect, test } from "vitest";

import type { WriteOp } from "$lib/api";
import { definirIdioma } from "$lib/i18n.svelte";
import { resumoDaOperacao } from "$lib/resumo";

afterEach(() => {
	definirIdioma("pt-BR");
});

const texto = (valor: string) => ({ text: valor, base64: null, truncated: false });

describe("resumo da operação", () => {
	test("campo de hash, nos três idiomas", () => {
		const op: WriteOp = { op: "set_hash_field", field: "saldo", value: texto("10") };

		expect(resumoDaOperacao("wallet:1001", op)).toBe(
			"Definir o campo “saldo” de wallet:1001",
		);

		definirIdioma("en");
		expect(resumoDaOperacao("wallet:1001", op)).toBe(
			"Set the field “saldo” of wallet:1001",
		);

		definirIdioma("es");
		expect(resumoDaOperacao("wallet:1001", op)).toBe(
			"Definir el campo “saldo” de wallet:1001",
		);
	});

	test("apagar a chave é a frase mais importante do conjunto", () => {
		definirIdioma("en");
		expect(resumoDaOperacao("user:1001", { op: "delete_key" })).toBe(
			"Delete the key user:1001",
		);
	});

	test("as duas pontas de uma lista são frases diferentes, não uma palavra trocada", () => {
		// Colar "no início"/"no final" dentro da frase funciona em português
		// e desmonta em qualquer idioma que ordene diferente.
		const inicio: WriteOp = { op: "push_list_item", value: texto("x"), front: true };
		const fim: WriteOp = { op: "push_list_item", value: texto("x"), front: false };

		definirIdioma("en");
		expect(resumoDaOperacao("fila:pedidos", inicio)).toBe(
			"Add an item at the start of fila:pedidos",
		);
		expect(resumoDaOperacao("fila:pedidos", fim)).toBe(
			"Add an item at the end of fila:pedidos",
		);
	});

	test("prazo posto e prazo tirado", () => {
		definirIdioma("en");
		expect(resumoDaOperacao("session:a", { op: "set_ttl", ttl_ms: 60000 })).toBe(
			"Set the TTL of session:a to 60000 ms",
		);
		expect(resumoDaOperacao("session:a", { op: "set_ttl", ttl_ms: null })).toBe(
			"Make session:a persistent (remove the TTL)",
		);
	});

	test("uma exclusão em massa diz o prefixo e a contagem", () => {
		const op: WriteOp = {
			op: "delete_prefix",
			prefixes: ["sessao:"],
			deleted: 1_200,
			enumerated: 1_200,
			backup: { backup: "inline" },
		};

		// `deleted`, o que saiu de verdade — não `enumerated`, que é o que a
		// varredura prometeu antes de alguém poder desistir no meio.
		expect(resumoDaOperacao("sessao:", op)).toBe(
			"Apagar 1.200 chaves sob “sessao:”",
		);

		definirIdioma("en");
		expect(resumoDaOperacao("sessao:", { ...op, deleted: 1 })).toBe(
			"Delete 1 key under “sessao:”",
		);
	});

	test("vários prefixos de uma vez não fingem ser um só", () => {
		const op: WriteOp = {
			op: "delete_prefix",
			prefixes: ["sessao:", "pedido:", "carrinho:"],
			deleted: 90,
			enumerated: 90,
			backup: { backup: "none" },
		};

		// Nomear só o primeiro faria a linha do histórico descrever menos do
		// que de fato aconteceu.
		expect(resumoDaOperacao("sessao:", op)).toBe("Apagar 90 chaves sob 3 prefixos");
	});

	test("membro binário não tem texto para mostrar, e a frase diz isso", () => {
		definirIdioma("en");
		const op: WriteOp = {
			op: "remove_zset_member",
			member: { text: null, base64: "AAEC", truncated: false },
		};

		expect(resumoDaOperacao("ranking:x", op)).toBe(
			"Remove the member “(binary value)” of ranking:x",
		);
	});
});
