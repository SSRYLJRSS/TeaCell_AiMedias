# Local gateway TLS fixture

`gateway-test-cert.pem`, `gateway-test-key.pem`, and `gateway-test-trust.pem` are a public, self-signed test identity for the loopback gateway tests. The key is intentionally checked in as test data; never reuse it for a provider, service, or any non-loopback endpoint.

The certificate is valid from 2020-01-01 through 2040-01-01 and includes the IP subject alternative name `127.0.0.1`. The test injects its certificate as a Rust trust root only when it starts the local HTTPS fixture. Tests without that explicit trust must fail before an HTTP request reaches the fixture.
