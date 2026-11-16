/**
 * A frase que descreve uma escrita, montada na tela.
 *
 * Ela nascia pronta no Rust, em português. Mas é o texto que a pessoa lê
 * **antes de autorizar uma mudança em produção** — a frase mais importante
 * do produto — e uma frase que não acompanha o idioma de quem lê é um
 * aviso perdido. O Rust continua gerando a sua versão para o registro do
 * histórico; quem fala com gente é esta.
 */
import type { Bytes, WriteOp } from "$lib/api";
import { t } from "$lib/i18n.svelte";

/** O texto de um valor, ou a marca de que ele é binário. */
function rotulo(bytes: Bytes): string {
	return bytes.text ?? t("valor.binario");
}

export function resumoDaOperacao(chave: string, op: WriteOp): string {
	switch (op.op) {
		case "set_string":
			return t("op.setString", { chave });
		case "set_hash_field":
			return t("op.setHashField", { campo: op.field, chave });
		case "delete_hash_field":
			return t("op.deleteHashField", { campo: op.field, chave });
		case "set_list_item":
			return t("op.setListItem", { indice: op.index, chave });
		case "push_list_item":
			// Duas frases inteiras, e não uma palavra trocada no meio: colar
			// "no início"/"no final" funciona em português e desmonta em
			// qualquer idioma que ordene os termos de outro jeito.
			return op.front
				? t("op.pushListItemInicio", { chave })
				: t("op.pushListItemFim", { chave });
		case "remove_list_item":
			return t("op.removeListItem", { indice: op.index, chave });
		case "set_zset_member":
			return t("op.setZSetMember", {
				membro: rotulo(op.member),
				chave,
				score: op.score,
			});
		case "remove_zset_member":
			return t("op.removeZSetMember", { membro: rotulo(op.member), chave });
		case "set_ttl":
			return op.ttl_ms === null
				? t("op.clearTtl", { chave })
				: t("op.setTtl", { chave, ms: op.ttl_ms });
		case "delete_key":
			return t("op.deleteKey", { chave });
	}
}
