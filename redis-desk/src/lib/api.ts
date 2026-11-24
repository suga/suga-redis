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

/** Um filho de um nível da árvore: um galho a abrir ou uma folha a ler. */
export interface Branch {
	/** O segmento, sem o prefixo do pai. */
	label: string;
	/** O prefixo acumulado, já com o `:` no fim. Abre este galho. */
	full_prefix: string;
	count: number;
	/** Preenchido só quando este filho É uma chave, não um galho. */
	leaf_key: string | null;
}

/** Um nível da árvore: os filhos e se a lista saiu inteira. */
export interface Level {
	children: Branch[];
	/** Há mais filhos distintos aqui do que o teto permite guardar. */
	truncated: boolean;
}

/** Um retrato da varredura em andamento. */
export interface ScanSnapshot {
	scanned: number;
	total_estimate: number;
	/** Os prefixos de primeiro nível achados até aqui, já contados. */
	root: Branch[];
	done: boolean;
}

/**
 * O mesmo retrato, dizendo de que (host, banco) ele veio. É o que deixa a
 * tela descartar um retrato que não é do par aberto, em vez de plantar as
 * chaves de um host na árvore de outro.
 */
export interface TreeProgress extends ScanSnapshot {
	host_id: string;
	db: number;
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
 * Uma chave que a volta atrás de uma exclusão em massa não conseguiu trazer
 * de volta, com o motivo já em texto.
 */
export interface FailedKey {
	key: string;
	reason: string;
}

/**
 * O que aconteceu com um pedido de escrita. `needs_confirmation` significa
 * que NADA foi escrito: é produção e falta confirmar. A tela mostra o
 * diálogo com `summary` e, se a pessoa autorizar, repete a mesma chamada
 * com `confirmed: true`. `deleted` e `restored` são as duas pontas da
 * exclusão em massa: uma exclusão que aconteceu, e a volta atrás dela.
 */
export type WriteOutcome =
	| { outcome: "applied"; entry_id: number }
	| { outcome: "needs_confirmation"; summary: string }
	| { outcome: "deleted"; entry_id: number; deleted: number; enumerated: number }
	| { outcome: "restored"; restored: number; failed: FailedKey[] };

// -------------------------------------------------------------- histórico

export type Actor = "human" | "llm";

export interface HistoryEntry {
	id: number;
	host_id: string;
	host_name: string;
	env: Env;
	db: number;
	key: string;
	/** A frase gravada quando a escrita aconteceu, em português. */
	summary: string;
	/**
	 * A operação que gerou a entrada. A tela monta a frase a partir dela, no
	 * idioma de quem lê; `null` nas entradas antigas, que só têm `summary`.
	 */
	op: WriteOp | null;
	before_json: string | null;
	after_json: string | null;
	actor: Actor;
	/** Epoch em milissegundos. */
	at: number;
	rolled_back: boolean;
}

// ------------------------------------------------------------- índice/busca

/** Andamento da varredura de nomes que monta a árvore. */
export interface ScanTick {
	scanned: number;
	/** `DBSIZE` lido antes de começar. Estimativa, não total exato. */
	total_estimate: number;
}

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

// ------------------------------------------- escrita pedida pelo Claude

/**
 * Um pedido de escrita que o Claude mandou pelo servidor MCP e que está
 * parado esperando uma decisão. Nada foi escrito enquanto ele existe.
 */
export interface ApprovalRequest {
	id: string;
	host_id: string;
	/** O texto que precisa ser digitado para liberar. */
	host_name: string;
	env: Env;
	db: number;
	key: string;
	summary: string;
	op: WriteOp;
	/** Epoch em milissegundos. */
	asked_at: number;
}

export type ApprovalVerdict = "allowed" | "denied" | "timed_out";

// ------------------------------------------------------------------ erros

/**
 * Todo comando falha assim. `kind` é o discriminante estável — a tela decide
 * por ele, nunca lendo o texto de `message`, que é copy e pode mudar.
 */
export interface ApiError {
	kind:
		| "unreachable"
		| "auth"
		| "cluster_single_db"
		| "protocol"
		| "key_gone"
		| "not_editable"
		| "ambiguous"
		| "config"
		| "index"
		| "history"
		| "unknown_host"
		| "name_mismatch"
		| "approval";
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

/**
 * Varre o banco UMA vez e guarda a árvore de prefixos.
 *
 * Não espere por ela para mostrar alguma coisa: acompanhe `onTreeProgress`,
 * que entrega a raiz parcial enquanto a varredura corre, e use `treeLevel`
 * para abrir galhos desde o primeiro retrato.
 */
export function scanTree(hostId: string, db: number): Promise<void> {
	return invoke("cmd_scan_tree", { hostId, db });
}

/**
 * Os filhos de um prefixo, lidos da árvore já varrida — sem ida ao
 * servidor. `null` quando este banco ainda não começou a ser varrido.
 */
export function treeLevel(
	hostId: string,
	db: number,
	prefix: string,
): Promise<Level | null> {
	return invoke("cmd_tree_level", { hostId, db, prefix });
}

/**
 * Carrega mais filhos de um galho cuja lista saiu incompleta.
 *
 * Varre de novo só aquele prefixo — caro, porque o servidor percorre o
 * espaço de chaves inteiro mesmo com `MATCH`. Por isso só quando pedido.
 */
export function loadMoreChildren(
	hostId: string,
	db: number,
	prefix: string,
): Promise<Level> {
	return invoke("cmd_load_more_children", { hostId, db, prefix });
}

/**
 * Acompanha a varredura que monta a árvore. Chame o retorno para parar.
 *
 * O evento é global: quem escuta recebe o retrato de QUALQUER varredura em
 * andamento, inclusive a de um host do qual já se desconectou (ela segue
 * correndo no Rust sobre a trie dela). Por isso o retrato vem endereçado —
 * cabe a quem escuta descartar o que não é do par aberto.
 */
export function onTreeProgress(
	handler: (retrato: TreeProgress) => void,
): Promise<UnlistenFn> {
	return listen<TreeProgress>("tree://progress", (event) => handler(event.payload));
}

// --------------------------------------------------- exclusão em massa

/**
 * O resumo de uma enumeração de prefixo — e só o resumo. A lista de nomes
 * fica no Rust; só a contagem e os tipos atravessam a fronteira.
 */
export interface EnumerationHandle {
	id: string;
	count: number;
	kinds: [KeyKind, number][];
	/** A varredura bateu no teto de 100.000 chaves antes de terminar. */
	truncated: boolean;
	/** Quantas são de tipo sem volta atrás (set, stream, RedisJSON). */
	unrestorable: number;
}

/** O que a pessoa escolheu no diálogo sobre a cópia de segurança. */
export type BackupRequest = "inline" | "file" | "none";

/**
 * Varre um ou mais prefixos e guarda os nomes achados, sem apagar nada
 * ainda. Acompanhe o andamento com `onBulkScanProgress`.
 *
 * Só existe UMA enumeração por par (host, banco): chamar de novo para o
 * mesmo par descarta a anterior — por isso só um diálogo de exclusão em
 * massa pode estar aberto por banco de cada vez.
 */
export function enumeratePrefix(
	hostId: string,
	db: number,
	prefixes: string[],
): Promise<EnumerationHandle> {
	return invoke("cmd_enumerate_prefix", { hostId, db, prefixes });
}

/**
 * Apaga as chaves de uma enumeração já feita. Em host de produção,
 * `typedHostName` é comparado byte a byte com o nome do host — sem
 * aparar espaços e sem ignorar maiúsculas/minúsculas; fora de produção
 * ele é ignorado. Acompanhe o andamento com `onBulkDeleteProgress`.
 *
 * Uma recusa (`needs_confirmation`) NÃO consome a enumeração: o mesmo
 * `enumerationId` continua válido, e a tela repete a chamada com o nome
 * digitado em vez de varrer tudo de novo.
 */
export function deletePrefix(
	enumerationId: string,
	backup: BackupRequest,
	typedHostName: string | null,
): Promise<WriteOutcome> {
	return invoke("cmd_delete_prefix", { enumerationId, backup, typedHostName });
}

/**
 * Pede para parar uma exclusão em massa em andamento. `false` não é falha:
 * pode ser que a exclusão já tenha acabado antes de o clique chegar.
 */
export function cancelDelete(enumerationId: string): Promise<boolean> {
	return invoke("cmd_cancel_delete", { enumerationId });
}

/** Acompanha a varredura de uma enumeração. Chame o retorno para parar. */
export function onBulkScanProgress(
	handler: (tick: ScanTick) => void,
): Promise<UnlistenFn> {
	return listen<ScanTick>("bulk://scan-progress", (event) => handler(event.payload));
}

/**
 * Acompanha a exclusão em massa já confirmada: quantas chaves já saíram.
 * Chame o retorno para parar.
 */
export function onBulkDeleteProgress(
	handler: (deleted: number) => void,
): Promise<UnlistenFn> {
	return listen<number>("bulk://delete-progress", (event) => handler(event.payload));
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

/** Os pedidos de escrita do Claude que estão esperando uma decisão. */
export function pendingApprovals(): Promise<ApprovalRequest[]> {
	return invoke("cmd_pending_approvals");
}

/**
 * Libera uma escrita pedida pelo Claude. `typedHostName` tem que ser
 * exatamente o nome do host — qualquer outra coisa falha com `name_mismatch`
 * e o pedido continua esperando.
 */
export function resolveApproval(
	requestId: string,
	typedHostName: string,
): Promise<ApprovalVerdict> {
	return invoke("cmd_resolve_approval", { requestId, typedHostName });
}

/** Recusa uma escrita pedida pelo Claude. */
export function denyApproval(requestId: string): Promise<ApprovalVerdict> {
	return invoke("cmd_deny_approval", { requestId });
}

/** Avisa que a fila de pedidos mudou. Chame a função devolvida para parar. */
export function onApprovalsChanged(
	handler: (pending: number) => void,
): Promise<UnlistenFn> {
	return listen<number>("approvals://changed", (event) => handler(event.payload));
}

// ------------------------------------------------------------------- log

export type LogKind = "connection" | "failure" | "slow";

/**
 * Uma linha do log em memória: uma conexão indo ou vindo, uma falha, ou uma
 * operação lenta. `host` nunca vem vazio — quando o id não é reconhecido, a
 * fachada grava o próprio id como rótulo.
 */
export interface LogEntry {
	at_ms: number;
	kind: LogKind;
	host: string;
	db: number | null;
	operation: string;
	duration_ms: number | null;
	detail: string | null;
}

/** O log inteiro, do mais novo para o mais velho. */
export function logs(): Promise<LogEntry[]> {
	return invoke("cmd_logs");
}

/** Conecta de verdade e devolve o tempo do PING em milissegundos. */
export function connect(hostId: string): Promise<number> {
	return invoke("cmd_connect", { hostId });
}

/** Encerra a conexão com o host. */
export function disconnect(hostId: string): Promise<void> {
	return invoke("cmd_disconnect", { hostId });
}
