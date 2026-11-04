#!/usr/bin/env bash
# Gera os certificados SÓ de teste usados pelos testes de TLS/mTLS do core.
# Não são versionados (.gitignore ignora *.pem/*.key): o CI roda este script
# antes dos testes; localmente, rode uma vez. A chave da CA é descartada.
set -euo pipefail
cd "$(dirname "$0")"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
dias=36500

openssl req -x509 -newkey rsa:2048 -nodes -days $dias -subj "/CN=Ferrix Test CA" \
  -keyout "$tmp/ca.key" -out ca.pem \
  -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" 2>/dev/null

assinar() { # nome CN extensões
  openssl req -newkey rsa:2048 -nodes -subj "/CN=$2" -keyout "$1.key" -out "$tmp/$1.csr" 2>/dev/null
  printf '%s\n' "$3" > "$tmp/$1.ext"
  openssl x509 -req -in "$tmp/$1.csr" -CA ca.pem -CAkey "$tmp/ca.key" -CAcreateserial -CAserial "$tmp/ca.srl" \
    -days $dias -extfile "$tmp/$1.ext" -out "$1.pem" 2>/dev/null
}

assinar server backend.internal "basicConstraints=CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=DNS:backend.internal,DNS:localhost,IP:127.0.0.1"

assinar client client "basicConstraints=CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=clientAuth"
