/** Catálogo em português do Brasil — a base. Ver `i18n.svelte.ts`. */
export const ptBR = {
	"estado.conectado": "conectado · {ms} ms · banco {banco}",
	"acao.conectar": "Conectar",
};

/** A forma de um catálogo: as chaves do português, todas. */
export type Catalogo = Record<keyof typeof ptBR, string>;
