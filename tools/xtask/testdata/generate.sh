#!/bin/sh
# Regenerates the smoke test's model server certificate (ADR-0054): a test
# CA and a certificate it issued for models.oceans.test, with its P-256
# key. Test material only: the keys are public, and only the smoke test's
# guest is told to trust the CA (`ai model ... --ca`). Run from this
# directory.
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
subjectAltName = DNS:models.oceans.test
[ca]
basicConstraints = critical,CA:TRUE
keyUsage = critical,keyCertSign,cRLSign
subjectKeyIdentifier = hash
EOF

openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out models.key.pem
openssl pkcs8 -topk8 -nocrypt -in models.key.pem -outform DER -out models.key.der
openssl req -new -key models.key.pem -subj "/CN=models.oceans.test" -config openssl.cnf -out models.csr
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out models-ca.key.pem
openssl req -x509 -new -key models-ca.key.pem -subj "/CN=Oceans test CA (models)" \
  -days 7300 -sha256 -config openssl.cnf -extensions ca -out models-ca.pem
openssl x509 -req -in models.csr -CA models-ca.pem -CAkey models-ca.key.pem \
  -set_serial "0x$(openssl rand -hex 8)" -days 3650 -sha256 \
  -extfile openssl.cnf -extensions server -outform DER -out models.der
rm models.csr models-ca.key.pem models.key.pem openssl.cnf
