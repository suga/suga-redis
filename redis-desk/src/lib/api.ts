/**
 * A única porta entre a interface e o Rust.
 *
 * Nenhum outro arquivo da interface chama `invoke` direto: todo componente
 * passa por aqui. Assim existe UM lugar onde o nome de um comando e o
 * formato dos dados precisam bater com o Rust — se `commands.rs` mudar, o
 * conserto é neste arquivo, não espalhado pelos componentes.
 *
 * Os tipos abaixo espelham os `struct`/`enum` de `src-tauri/src/core/` com
 * os nomes de campo COMO O SERDE OS ESCREVE (snake_case). Os nomes dos
 * ARGUMENTOS, não: o Tauri converte camelCase daqui para snake_case lá.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// ----------------------------------------------------------------- hosts

export type Env = "prod" | "homolog" | "dev";

export interface Host {
	id: string;
	name: string;
	addr: string;
	port: number;
	db: number;
	username: string | null;
	tls: boolean;
	env: Env;
}

// ----------------------------------------------------------------- chaves

/** Tipo de uma chave. `{ unknown: "..." }` é um tipo de módulo que não conhecemos. */
export type KeyKind =
	| "string"
	| "hash"
	| "list"
	| "zset"
	| "set"
	| "stream"
	| "json"
	| { unknown: string };

/** Os quatro tipos que este produto sabe escrever de volta. */
const EDITABLE_KINDS = ["string", "hash", "list", "zset"];

export function isEditable(kind: KeyKind): boolean {
	return typeof kind === "string" && EDITABLE_KINDS.includes(kind);
}

export interface KeyMeta {
	key: string;
	kind: KeyKind;
	ttl_ms: number | null;
	size_bytes: number;
}

export interface TreeNode {
	label: string;
	full_prefix: string;
	count: number;
	children: TreeNode[];
	/** Preenchido só nas folhas: o nome completo da chave. */
	leaf_key: string | null;
}

// ------------------------------------------------------------------ valor

/**
 * Um pedaço de dado do Redis. `text` quando é UTF-8 válido, `base64` quando
 * não é — nunca os dois, e nunca uma conversão que corromperia bytes.
 */
export interface Bytes {
	text: string | null;
	base64: string | null;
	truncated: boolean;
}

export interface StreamEntry {
	id: string;
	fields: [string, Bytes][];
}

export type Value =
	| { kind: "string"; data: Bytes }
	| { kind: "hash"; data: [string, Bytes][] }
	| { kind: "list"; data: Bytes[] }
	| { kind: "zset"; data: [Bytes, number][] }
	| { kind: "set"; data: Bytes[] }
	| { kind: "stream"; data: StreamEntry[] }
	| { kind: "read_only_text"; data: string };

export interface ValueView {
	key: string;
	kind: KeyKind;
	ttl_ms: number | null;
	size_bytes: number;
	value: Value;
	total_elements: number;
	/** O que veio não é o valor inteiro: há mais sob pedido. */
	truncated: boolean;
}

/** Uma mudança tipada. É o ÚNICO jeito de a interface escrever no Redis. */
export type WriteOp =
	| { op: "set_string"; value: Bytes }
	| { op: "set_hash_field"; field: string; value: Bytes }
	| { op: "delete_hash_field"; field: string }
	| { op: "set_list_item"; index: number; value: Bytes }
	| { op: "push_list_item"; value: Bytes; front: boolean }
	| { op: "remove_list_item"; index: number }
	| { op: "set_zset_member"; member: Bytes; score: number }
	| { op: "remove_zset_member"; member: Bytes }
	| { op: "set_ttl"; ttl_ms: number | null }
	| { op: "delete_key" };

/** Atalho para montar um `Bytes` a partir de texto digitado na tela. */
export function textBytes(text: string): Bytes {
	return { text, base64: null, truncated: false };
}

/**
 * O que aconteceu com um pedido de escrita. `needs_confirmation` significa
 * que NADA foi escrito: é produção e falta confirmar. A tela mostra o
 * diálogo com `summary` e, se a pessoa autorizar, repete a mesma chamada
 * com `confirmed: true`.
 */
export type WriteOutcome =
	| { outcome: "applied"; entry_id: number }
	| { outcome: "needs_confirmation"; summary: string };

// -------------------------------------------------------------- histórico

export type Actor = "human" | "llm";

export interface HistoryEntry {
	id: number;
	host_id: string;
	host_name: string;
	env: Env;
	db: number;
	key: string;
	summary: string;
	before_json: string | null;
	after_json: string | null;
	actor: Actor;
	/** Epoch em milissegundos. */
	at: number;
	rolled_back: boolean;
}

// ------------------------------------------------------------- índice/busca

export interface ScanProgress {
	scanned: number;
	total_estimate: number;
}

export interface ScanReport {
	host_id: string;
	db: number;
	keys: number;
	finished_at: number;
	duration_ms: number;
}

export type Where = "key_name" | "value";

export interface SearchHit {
	key: string;
	kind: KeyKind;
	ttl_ms: number | null;
	where_found: Where;
	snippet: string;
	/** Deslocamentos em BYTES dentro de `snippet`, para grifar o acerto. */
	match_start: number;
	match_len: number;
}

export interface SearchOutcome {
	hits: SearchHit[];
	scanned: number;
	elapsed_ms: number;
	stale_keys_removed: number;
	/** Idade do índice em segundos, ou -1 se nunca foi varrido. */
	index_age_secs: number;
}

/** `index_age_secs` quando o (host, banco) nunca foi varrido. */
export const NEVER_SCANNED = -1;

// ------------------------------------------------------------------ erros

/**
 * Todo comando falha assim. `kind` é o discriminante estável — a tela decide
 * por ele, nunca lendo o texto de `message`, que é copy e pode mudar.
 */
export interface ApiError {
	kind:
		| "unreachable"
		| "auth"
		| "protocol"
		| "key_gone"
		| "not_editable"
		| "ambiguous"
		| "config"
		| "index"
		| "history"
		| "unknown_host";
	message: string;
}

/**
 * Reconhece o erro vindo do Rust. O `catch` do TypeScript é `unknown`, e uma
 * falha do próprio `invoke` (processo morto, comando não registrado) não tem
 * este formato — por isso a checagem em vez de um cast.
 */
export function isApiError(err: unknown): err is ApiError {
	return (
		typeof err === "object" &&
		err !== null &&
		"kind" in err &&
		"message" in err
	);
}

/** A frase a mostrar para qualquer coisa que der errado, inclusive o inesperado. */
export function errorMessage(err: unknown): string {
	if (isApiError(err)) return err.message;
	return String(err);
}

// --------------------------------------------------------------- comandos

export function listHosts(): Promise<Host[]> {
	return invoke("cmd_list_hosts");
}

export function addHost(host: Host, password: string | null): Promise<string> {
	return invoke("cmd_add_host", { host, password });
}

export function updateHost(host: Host, password: string | null): Promise<void> {
	return invoke("cmd_update_host", { host, password });
}

export function removeHost(hostId: string): Promise<void> {
	return invoke("cmd_remove_host", { hostId });
}

/** Testa a conexão, devolvendo o tempo de resposta em milissegundos. */
export function testHost(hostId: string): Promise<number> {
	return invoke("cmd_test_host", { hostId });
}

/** Os bancos com ao menos uma chave, como pares `[banco, quantidade]`. */
export function listDbs(hostId: string): Promise<[number, number][]> {
	return invoke("cmd_list_dbs", { hostId });
}

export function loadTree(hostId: string, db: number): Promise<TreeNode[]> {
	return invoke("cmd_load_tree", { hostId, db });
}

/** Refaz o índice de busca. Acompanhe o andamento com `onRescanProgress`. */
export function rescan(hostId: string, db: number): Promise<ScanReport> {
	return invoke("cmd_rescan", { hostId, db });
}

export function scanStatus(
	hostId: string,
	db: number,
): Promise<ScanReport | null> {
	return invoke("cmd_scan_status", { hostId, db });
}

/** Escuta o progresso da varredura. Chame a função devolvida para parar. */
export function onRescanProgress(
	handler: (progress: ScanProgress) => void,
): Promise<UnlistenFn> {
	return listen<ScanProgress>("rescan://progress", (event) =>
		handler(event.payload),
	);
}

export function readValue(
	hostId: string,
	db: number,
	key: string,
): Promise<ValueView> {
	return invoke("cmd_read_value", { hostId, db, key });
}

/** A próxima fatia de uma coleção grande. */
export function readValueMore(
	hostId: string,
	db: number,
	key: string,
	offset: number,
): Promise<ValueView> {
	return invoke("cmd_read_value_more", { hostId, db, key, offset });
}

/**
 * Aplica uma mudança. Em host de produção, a primeira chamada deve ir com
 * `confirmed: false`: se voltar `needs_confirmation`, mostre o diálogo e só
 * então repita com `confirmed: true`.
 */
export function writeValue(
	hostId: string,
	db: number,
	key: string,
	op: WriteOp,
	confirmed: boolean,
): Promise<WriteOutcome> {
	return invoke("cmd_write_value", { hostId, db, key, op, confirmed });
}

export function listHistory(
	hostId: string,
	limit: number,
): Promise<HistoryEntry[]> {
	return invoke("cmd_list_history", { hostId, limit });
}

/** Desfaz uma entrada do histórico. Mesma regra de confirmação da escrita. */
export function rollback(
	entryId: number,
	confirmed: boolean,
): Promise<WriteOutcome> {
	return invoke("cmd_rollback", { entryId, confirmed });
}

export function search(
	hostId: string,
	db: number,
	needle: string,
): Promise<SearchOutcome> {
	return invoke("cmd_search", { hostId, db, needle });
}
