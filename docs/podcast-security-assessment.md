# Podcast transport security assessment (t-110)

Date: 2026-11-01. Status: both scoped advisory exceptions explicitly authorized by the user on 2026-11-01.

The current dependency audit reports two vulnerabilities in the selected,
previously linked transport dependency graph. The two authorized exceptions are configured in deny.toml and .cargo/audit.toml. This is a reachability assessment of the current
configuration, not a claim that the upstream crates are fixed.

## RUSTSEC-2026-0387: crypto-bigint 0.5.5

[The RustSec advisory](https://rustsec.org/advisories/RUSTSEC-2026-0387.html)
describes timing variability involving secret integers; the patched version is
0.7.6 or newer. The selected reqwless 0.14 HTTP parser unconditionally depends
on p256 0.13 and crypto-bigint 0.5.5. Its TLS client and p256 signing imports
are behind its `embedded-tls` feature, which Theo disables with
`default-features = false`. Theo uses its low-level HTTP request/response
interfaces over a separate verified TLS connection. Its actual elliptic-curve
provider is the pinned embassy crypto driver, using the newer dependency line.

Source checks: reqwless 0.14 `src/client.rs` gates its p256 signing import and
TLS client on `feature = "embedded-tls"`; root Cargo.toml disables that feature.
The complete Xtensa dependency tree confirms this is the only path to crypto-bigint 0.5.5.
A future enablement of reqwless's TLS feature or another use of the vulnerable
arithmetic on secret values invalidates this assessment.

## RUSTSEC-2023-0071: rsa 0.9.10

[The RustSec advisory](https://rustsec.org/advisories/RUSTSEC-2023-0071.html)
describes private-key recovery through timing of RSA private operations. No
patched version is currently listed. The selected embedded-tls verifier uses
`RsaPublicKey`, PKCS#1/PSS verifying keys and `verify` for public certificate
and handshake signature verification. Theo configures no client certificate or
private key, and does not use RSA signing/decryption. The documented private-key
leak therefore has no corresponding private key in this configuration.

Source checks: selected embedded-tls revision
`6487c62bb084c33361822022e21836ddfd48388f`, `src/webpki/algorithms.rs`,
`rsa_key`, `rsa_pkcs1`, and `rsa_pss`; firmware StrictVerifier and TlsConfig
construction in `src/device/network.rs`. Adding client authentication or any RSA
private operation invalidates this assessment.

## Authorized audit treatment

Allow only these two advisory IDs in cargo-deny and cargo-audit, with reasons
linking this file and the conditions above. Keep all other vulnerability,
license and source checks enabled. Reassess on a dependency/feature change or
before 2026-12-01. The exceptions do not disable certificate-chain, hostname,
signature or date verification. Any future use outside these conditions requires a new assessment; the upstream
vulnerabilities remain unfixed.

The additional 0BSD license used by the selected networking dependencies is a
permissive license, consistent with this project's permissive dependency policy;
allowing it does not suppress security advisories.
