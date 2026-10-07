# Schema-2 checkpoint fixture

MIT-licensed metadata produced from this repository's clear AVC/AAC and subtitle
fixtures. It contains no resource URLs, raw keys or media/cue payloads.

`engine-schema2.bin` is the frozen 1.0 binary envelope, SHA-256
`81130c105a7d0383fe6008132feba20800688db5944c14dbbcbe6a02f095c9d5`.
The ignored `generate_schema2_fixture` integration helper records its provenance;
it must not be run to update the existing fixture after schema freeze. A future
incompatible format needs a new schema/fixture and an explicit migration path.

The checkpoint's destination digest intentionally binds the original test output.
Binary/serde compatibility is checked against this fixture. Actual file replay,
identity verification and sample reconstruction are covered by the separate
194-output checkpoint matrix and process-crash tests.
