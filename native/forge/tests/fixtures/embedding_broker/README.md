These DER certificates and the unencrypted PKCS8 private key are **public test
material**, generated exclusively for the native embedding broker's loopback TLS
tests. They must never be used by a live service or installed as trusted roots.

The test CA signs a server certificate for `localhost` and `127.0.0.1`, valid for
100 years from generation on 2026-11-01. Both use RSA-2048 and SHA-256. Tests add
this CA only to a private test client; the production WebPKI client must reject it.
OpenSSL generated the fixtures; tests read DER directly and need no OpenSSL binary.
