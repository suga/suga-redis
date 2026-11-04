/** Formatação de números e tempo para a tela, em português do Brasil. */

/** Quanto falta para a chave expirar, em palavras curtas. */
export function formatarPrazo(ms: number): string {
	if (ms <= 0) return "expirada";
	const segundos = Math.floor(ms / 1000);
	const s = segundos % 60;
	const m = Math.floor(segundos / 60) % 60;
	const h = Math.floor(segundos / 3600);
	if (h > 0) return `${h} h ${m} min`;
	if (m > 0) return `${m} min ${s} s`;
	return `${s} s`;
}

/** Tamanho em bytes na unidade que couber. */
export function formatarTamanho(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} kB`;
	return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** Há quanto tempo, a partir de uma duração em segundos. */
export function haQuantoTempo(segundos: number): string {
	if (segundos < 60) return "agora há pouco";
	const minutos = Math.round(segundos / 60);
	if (minutos < 60) return `há ${minutos} min`;
	const horas = Math.round(minutos / 60);
	if (horas < 24) return `há ${horas} h`;
	return `há ${Math.round(horas / 24)} dias`;
}

/** Número com os separadores do português do Brasil. */
export function numero(valor: number): string {
	return valor.toLocaleString("pt-BR");
}

/**
 * Parte `snippet` nos três pedaços em volta do acerto.
 *
 * `start` e `len` vêm do Rust em BYTES de UTF-8, e uma string de JavaScript
 * é indexada em unidades UTF-16 — cortar direto por esses números partiria
 * um acento ao meio. Por isso o recorte acontece sobre os bytes.
 */
export function partirNoAcerto(
	snippet: string,
	start: number,
	len: number,
): { antes: string; acerto: string; depois: string } {
	const bytes = new TextEncoder().encode(snippet);
	const decoder = new TextDecoder();
	return {
		antes: decoder.decode(bytes.slice(0, start)),
		acerto: decoder.decode(bytes.slice(start, start + len)),
		depois: decoder.decode(bytes.slice(start + len)),
	};
}
