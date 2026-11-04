#!/usr/bin/env bash
# Sobe/derruba um Redis de teste isolado para desenvolvimento e testes de
# integração do suga-redis. Container: suga-redis-test, porta 127.0.0.1:6399.
#
# Este é o ÚNICO lugar do repositório onde FLUSHALL pode aparecer, e mesmo
# assim só contra o container de teste (porta 6399). Em qualquer outro lugar
# do suga-redis, KEYS, FLUSHALL e FLUSHDB são proibidos.
set -euo pipefail

CONTAINER_NAME="suga-redis-test"
TEST_PORT="6399"
TEST_HOST="127.0.0.1"
REDIS_IMAGE="redis:7-alpine"

usage() {
    echo "Uso: $0 {up|down|reset}" >&2
    exit 1
}

container_exists() {
    docker ps -a --format '{{.Names}}' | grep -Fxq "${CONTAINER_NAME}"
}

container_running() {
    docker ps --format '{{.Names}}' | grep -Fxq "${CONTAINER_NAME}"
}

up() {
    if container_running; then
        echo "Container ${CONTAINER_NAME} já está rodando em ${TEST_HOST}:${TEST_PORT}."
        return 0
    fi

    if container_exists; then
        echo "Removendo container parado ${CONTAINER_NAME}..."
        docker rm "${CONTAINER_NAME}" >/dev/null
    fi

    echo "Subindo ${CONTAINER_NAME} (${REDIS_IMAGE}) em ${TEST_HOST}:${TEST_PORT}..."
    docker run -d \
        --name "${CONTAINER_NAME}" \
        -p "${TEST_HOST}:${TEST_PORT}:6379" \
        "${REDIS_IMAGE}" >/dev/null
    echo "Container ${CONTAINER_NAME} no ar."
}

down() {
    if container_exists; then
        echo "Removendo ${CONTAINER_NAME}..."
        docker rm -f "${CONTAINER_NAME}" >/dev/null
        echo "Container ${CONTAINER_NAME} removido."
    else
        echo "Container ${CONTAINER_NAME} não existe."
    fi
}

reset() {
    if ! container_running; then
        echo "Container ${CONTAINER_NAME} não está rodando. Rode '$0 up' primeiro." >&2
        exit 1
    fi

    echo "Limpando dados do container de teste ${CONTAINER_NAME} (porta ${TEST_PORT})..."
    docker exec "${CONTAINER_NAME}" redis-cli FLUSHALL >/dev/null
    echo "Container de teste limpo."
}

command="${1:-}"
case "${command}" in
    up)
        up
        ;;
    down)
        down
        ;;
    reset)
        reset
        ;;
    *)
        usage
        ;;
esac
