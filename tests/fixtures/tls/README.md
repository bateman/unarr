A self-signed certificate for `selfsigned.test` and its key, made for the
tests of the certificate validation setting (`crates/unarr-core/src/http.rs`):

    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
        -keyout selfsigned.key.pem -out selfsigned.cert.pem -days 36500 \
        -subj "/CN=selfsigned.test" -addext "subjectAltName=DNS:selfsigned.test"

The key protects nothing: no trust store holds the certificate.
