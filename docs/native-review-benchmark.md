# Native review benchmark

`forge candidate benchmark prepare` captures HEAD (or an explicitly selected staged index) and creates an isolated fixture containing the baseline plus named review scenarios. Preparation does not run a reviewer:

```sh
forge candidate benchmark prepare --repo . --fixture /data/review-fixture
```

The fixture manifest records source identity, scenario trees and expected review profiles. Review execution is separately opted into and requires a manifest digest, operator grants naming the exact fixture source root, an operator session owner, explicit output location, and the required pinned review inputs:

```sh
forge candidate benchmark run --fixture /data/review-fixture \
  --fixture-sha256 <prepared-manifest-sha256> --execute \
  --grants /path/to/fixture-grants.json --owner <operator-session> \
  --output /data/review-results <required-review-input-pins>
```

Preparation initializes only its newly created snapshot's Git metadata with
private directory permissions. Output parents and existing report files must
belong to the current user and deny group/world writes; see the
[shared publisher contract](native-candidate-review.md).

The six default scenarios include fast/full profiles and run each scenario in fresh cold then warm processes. These labels describe process/cache ordering; they do not control or measure the operating-system page cache. The benchmark invokes the real native review coordinator. It does not use a Python coordinator or GNU `time`. Optional Python equivalence guests and their hashes are supplied explicitly; guest-based checks remain external compatibility inputs, not native Rust orchestration.

Each completed measurement records elapsed child lifecycle and Linux `wait4` RSS. `max_rss_kib` means the largest individual waited-process RSS peak propagated through completed waits; unpropagated descendants are excluded. It is not simultaneous aggregate process-tree RSS. A benchmark completion means requested measurements completed, not that the reviews passed, that approval was granted, or that model quality improved. Review decisions and `approval` remain separate fields; checksums establish consistency, not authenticated approval.

The output is incrementally published. A failed or interrupted run is marked incomplete and preserves completed rows and recovery information. Forced cleanup failure can leave granted scratch/cgroup state requiring operator inspection; the fixture is retained. The measurements remain workload- and host-specific and do not establish a general latency or memory bound.
