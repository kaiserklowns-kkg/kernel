#!/bin/sh
# Regenerates the TLS test certificates (ADR-0031). Test material only: the
# keys are public. One P-256 server key, certified by a CA of each
# signature algorithm the provider verifies. Run from this directory.
set -e
export MSYS_NO_PATHCONV=1

cat > openssl.cnf <<'EOF'
[req]
distinguished_name = dn
[dn]
[server]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature
extendedKeyUsage = serverAuth
subjectAltName = IP:10.0.2.2,IP:127.0.0.1,DNS:localhost,DNS:oceans.test
[ca]
basicConstraints = critical,CA:TRUE
keyUsage = critical,keyCertSign,cRLSign
subjectKeyIdentifier = hash
EOF

openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out server.key.pem
openssl pkcs8 -topk8 -nocrypt -in server.key.pem -outform DER -out server.key.der
openssl req -new -key server.key.pem -subj "/CN=oceans.test" -config openssl.cnf -out server.csr

# make_ca NAME KEYGEN-ARGS SIGN-ARGS
make_ca() {
  openssl genpkey $2 -out "$1-ca.key.pem"
  openssl req -x509 -new -key "$1-ca.key.pem" -subj "/CN=Oceans test CA ($1)" \
    -days 7300 $3 -config openssl.cnf -extensions ca -out "$1-ca.pem"
  openssl x509 -in "$1-ca.pem" -outform DER -out "$1-ca.der"
  openssl x509 -req -in server.csr -CA "$1-ca.pem" -CAkey "$1-ca.key.pem" \
    -set_serial "0x$(openssl rand -hex 8)" -days 3650 $3 \
    -extfile openssl.cnf -extensions server -out "server-$1.pem"
  openssl x509 -in "server-$1.pem" -outform DER -out "server-$1.der"
  rm "$1-ca.key.pem"
}

make_ca ecdsa "-algorithm EC -pkeyopt ec_paramgen_curve:P-384" "-sha384"
make_ca rsa "-algorithm RSA -pkeyopt rsa_keygen_bits:2048" "-sha256"
make_ca pss "-algorithm RSA -pkeyopt rsa_keygen_bits:2048" \
  "-sha256 -sigopt rsa_padding_mode:pss -sigopt rsa_pss_saltlen:digest"
make_ca ed25519 "-algorithm ED25519" ""

rm server.csr openssl.cnf
