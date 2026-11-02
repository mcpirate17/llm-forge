# Radon6.0.1 primary-source expectations

`upstream-6.0.1.json` contains61 literal expectations mechanically extracted from the installed Radon6.0.1 test_complexity_visitor.py. Source strings use upstream's textwrap.dedent(...).strip() normalization. The CPython>=3.5 async function append is included; this target protocol is CPython3.12. No Radon module was imported and no tests or reference analysis were executed while extracting these data.

Copyright(c)2012-2017 Michele Lacchia, MIT; full license is retained at ../../../licenses/radon-MIT.txt. These are metric expectation data, not mutation campaigns or PASS artifacts.

Primary installed source byte identities:

- `radon/visitors.py` SHA256 `305939b058a4100787b47ca302b71afa827908c1471ccb568070c4ec0026ade5`
- `radon/complexity.py` SHA256 `48a34bc19a57908c99b2395c49f5c0539c26391d88842b966494b221091ca688`
- `radon/tests/test_complexity_visitor.py` SHA256 `9dc298582cd8e48807ede207371bc52c8c743f316c20a3c56de61aba7ed28034`
- `radon-6.0.1.dist-info/LICENSE` SHA256 `0270e0ab58f8f85385f6dbffed02c6465f8a3195afed605149019442233736ed`

Both Forge and LLM environments have byte-identical Radon visitors.py/complexity.py. At initial draft time the development-only check_reference.py had not run. On 2026-11-02 it ran against installed CPython 3.12.3/Radon 6.0.1 and all 61 original literal expectations agreed. The independent executed edge oracle below was also captured; native unit execution remains a separate validation step.


## Executed independent edge reference capture (2026-11-02)

The installed CPython 3.12.3/Radon 6.0.1 reference check has now run and all 61
original literal expectations agreed. The historical extraction statements above
refer to the earlier draft stage; they do not claim a native test run.

`edge-reference-6.0.1.json` contains 25 authored edge sources and both no_assert
variants (50 records), with actual ComplexityVisitor module complexity and the
complete raw cc_visit block projection, or actual CPython refusal. It was derived
byte-for-byte from SHA-bound source snapshots and actual reference capture
SHA256 a548ea20fa2e42dd32dfafcd52543dd4efdf29d8ec5b7fcaaa9116f6acd5cfb4,
frozen BEFORE native outputs were observed. Expected numeric values are reference
outputs, not values computed by the native implementation or hand-adjusted after
comparison. The installed source SHA identities match those above.

The native unit loop compares raw block order/all fields and module complexity
under both assertion options. Expected reference refusals must return Err, never
partial rows. It includes the observed UTF-8 BOM incompatibility as an independent
oracle refusal. New authored cases are not copied Radon implementation code; the
existing MIT notice remains applicable to the adapted visitor/upstream fixture.
Seven live tracked Python source comparisons remain external evidence and are NOT
archived in this fixture. These data do not assert complete grammar parity,
performance, mutation campaign PASS, runtime execution identity or approval.
