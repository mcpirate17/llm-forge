# Confined Fest campaigns

On Linux, `fest-confined-v1` is an explicit native-generated profile for one changed Python source per campaign. It runs Fest, pytest and attribution in a disposable snapshot under the existing confined process owner. It requires an operator-provisioned private Python guest and a separate, pinned execution grant. It does not provision Python or Fest, discover a host virtual environment, or grant execution authority from an inventory.

## Prepare the guest and inventory

Provision a private, current-user-owned `0700` prefix using your normal trusted installation process. The currently supported layout has exactly one `lib/python3.12` tree, including `lib-dynload` and `dist-packages`. It must contain regular, independently pinned `bin/python` and `bin/fest`, pytest, `_pytest`, pytest-cov and coverage, including the entry modules required by the native validator. The guest must contain the interpreter's standard library and native extension modules needed by those packages. Provisioning and dependency selection are operator responsibilities; an existing external Python/Fest installation is not Forge-owned.

The inventory rejects symlinks, hard links, `.pth` files, cached bytecode, `pyvenv.cfg`, and startup customization modules. The prefix is bounded to 10,000 entries, 256 MiB total and 32 MiB per file. The inventory is a point-in-time content check, not a signature or proof of package provenance. Output and prefix must be canonical, disjoint absolute paths; the output parent must already be a current-user-owned `0700` directory. The command creates a new file and does not overwrite an existing manifest:

```sh
forge mutation fest-guest \
  --root /operator/private/fest-prefix \
  --output /operator/private/fest-guest-manifest.json
```

Record the returned `manifest_sha256` and preserve the exact output bytes. The schema is `{ "schema_version": 1, "entries": [...] }`; each sorted entry contains a relative `path`, boolean `directory`, integer `bytes`, `sha256` (null for a directory, lowercase digest for a file), and boolean `executable`. Do not edit the generated inventory. Forge rechecks the pinned manifest and the complete guest inventory before and after use. A successful export does not execute or validate the guest's behavior.

## Grant and run

Create an operator-owned JSON grant, pin its exact bytes, and choose an existing delegated cgroup v2 parent and a private scratch parent. The example is a shape, not an executable grant; replace every placeholder with a canonical absolute path or independently measured lowercase SHA-256. `read_only` must contain the exact guest root plus each exact external runtime file needed by the interpreter and Fest. `python_guest.external_files` must have exactly the same external file paths and independently measured hashes. Broad system-library directory grants are not accepted for this profile. Include only files your reviewed toolchain needs; identify the ELF interpreter, shared libraries and loader cache on your host rather than assuming this abbreviated example is complete.

```json
{
  "version": 1,
  "source_root": "/absolute/repository",
  "scratch_parent": "/operator/private/scratch",
  "cgroup_parent": "/sys/fs/cgroup/your-delegated-parent",
  "read_only": ["/operator/private/fest-prefix", "/absolute/required-runtime-file"],
  "programs": {
    "python": {"path": "/operator/private/fest-prefix/bin/python", "sha256": "PYTHON_SHA256"},
    "fest": {"path": "/operator/private/fest-prefix/bin/fest", "sha256": "FEST_SHA256"}
  },
  "resources": {"cpu_millis": 1000, "memory_mib": 1024, "processes": 64},
  "max_wall_seconds": 120,
  "python_guest": {
    "root": "/operator/private/fest-prefix",
    "manifest_path": "/operator/private/fest-guest-manifest.json",
    "manifest_sha256": "GUEST_MANIFEST_SHA256",
    "external_files": {"/absolute/required-runtime-file": "RUNTIME_FILE_SHA256"}
  }
}
```

The guest root, manifest, executables, external files, source, scratch and cgroup are independently admitted with ownership, identity, alias and overlap checks. The example budgets are limits, not recommendations or measured usage. A positive explicit legacy per-process memory cap is refused: the receipt describes the actual aggregate cgroup limit. Supply a wall bound that covers each permitted child command and a separate outer operational deadline appropriate for the full run.

Use the native writer rather than editing a campaign or receipt. The source must be changed in the selected scope; choose a base and owner appropriate for the branch. Review the plan before writing:

```sh
forge mutation plan python --repo-root /absolute/repository --owner my-lane \
  --base origin/main --only path/to/changed.py --fest-confined --python-jobs 1 --run-timeout 120
forge mutation write python --repo-root /absolute/repository --owner my-lane \
  --base origin/main --only path/to/changed.py --fest-confined --python-jobs 1 --run-timeout 120
forge mutation run campaigns/PATH-FROM-WRITE.json --repo-root /absolute/repository \
  --owner my-lane --base origin/main --allow-mutations \
  --receipt campaigns/receipts/my-run.json \
  --execution-grants /operator/private/grants.json \
  --execution-grants-sha256 SHA256_OF_EXACT_GRANT_BYTES
```

Use the manifest path actually returned by `write`, and a receipt path permitted by the captured project configuration. The profile requires one source, selected pinned Python tests, one worker, an empty campaign environment and the generated pytest arguments. It refuses unsupported pytest/Fest/coverage configuration, conftest and startup hooks rather than silently changing the test scope. Other projects may need a different profile before they can run.

## Isolation and evidence

Forge holds and checks the guest and exact external files, derives a trusted-first import path, disables pytest plugin autoload, and uses pytest's importlib mode. This protects imports from ordinary repository-name collisions for modules present in the pinned guest. Tests are arbitrary code and can deliberately change their own imports; this is not hostile-test attestation. The child has no network access and receives only admitted read paths and private writable scratch.

Bytecode writes are disabled and `PYTHONPYCACHEPREFIX` points to an initially empty, read-only sibling outside writable scratch. Its identity and emptiness are checked, and its removal is recorded only after owned processes and input verification complete. This differs from the legacy Fest plugin/eviction path described in the historical bytecode section of [mutation.md](mutation.md). Unknown cleanup or changed inputs cause refusal and may retain the snapshot or cache for inspection; receipt paths are diagnostic, not authority to delete them.

A marked Fest PASS requires normal successful baseline runs, complete owned process and cache cleanup, and a complete JUnit-backed attribution for every engine-reported kill. Attribution reruns require normal pytest test-failure exit status and selected failed nodes; setup/collection errors, missing XML and timeouts do not establish kills. Retained normalized observations and the recomputed test-value analysis are consistency evidence, not independent producer attestation. Survivors, incomplete runs, drift and `RATCHET_HELD` are not PASS. Review the generated receipt and source/test pins; the mutation run does not itself approve the candidate or validate every possible Python workload.
