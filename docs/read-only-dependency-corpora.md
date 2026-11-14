# Keep reviewed dependency corpora out of source archives

Native acceptance is pending. Local builds and live verification remain queued
behind the owning QA run; remote CI alone does not establish installed behavior.
Do not alter an already frozen arm to adopt this option.

Large vendored dependencies can exhaust the unchanged8MiB compressed workspace
archive cap. The bench does **not** exclude directories merely named `vendor`.
Use an explicitly reviewed dependency-only corpus, retained on the host and
mounted read-only at its exact declared destination. For example:

```json
{"read_only_dependency_corpora":[{
  "host_dir":"/private/case/dependencies",
  "container_path":"/workspace/vendor",
  "sha256":"REPLACE_WITH_REVIEWED_TREE_SHA256"
}]}
```

The field defaults to an empty list. At most four corpora are supported.
Destinations must be strict descendants of the container workspace or `/deps`.
They cannot cover the program workspace, pinned artifacts, runtime state or
secrets; host sources cannot cover private operator inputs or the workspace root.

Freeze and retain the corpus, its regeneration recipe, matching dependency lock
files and provenance before the case. Hash the actual idle container tree with
the rebuilt, pinned `mc-sdk-runtime-check dependency-witness ABSOLUTE_DIRECTORY`
and independently review that it contains only regenerable dependencies. The
digest pins relative paths, entry types, permission bits and file bytes in sorted
tree order. It does not depend on the absolute mount path or certify arbitrary
contents credential-free. The helper makes no model, SDK or gameplay calls.

## Required mount and identity checks

Preflight requires one exact read-only bind with the declared canonical host
source and container destination. Nested mounts are rejected. A writable alias
to the same source is rejected; a writable parent workspace is allowed only when
the corpus's mapped child path is masked by this exact read-only mount. The
pinned helper must support the new command; older helper binaries fail before
the actor connects when this option is used.

Each actual-tree witness is bounded to65536 entries,2GiB, depth32 and a15-second
cooperative scan deadline, with a30-second host command deadline.
Entry names are reserved against one global discovery budget before buffering;
the successful witness still reports the visited-entry count in sorted tree order.
Symlinks, hard-linked regular files, special files, unreadable entries and detected
in-flight changes are incomplete. Roots and filenames must be canonical/UTF8.
Dependencies needing source-tree writes are unsupported: keep build outputs in
the ordinary writable target/cache instead of weakening the read-only mount.

After proven handler/actor closure, the archive rechecks the frozen container
generation and mount declaration and obtains a fresh matching tree witness.
Only then can it add that literal path under tar's anchored/no-wildcard mode.
It rechecks mounts, root device/inode and the pinned digest after capture.
`archive-dependency-corpora.json` records the witnesses; `workspace-archive.json`
reports dependency verification separately. Corpus bytes are not duplicated into
the evidence bundle. The operator must retain the host corpus and provenance.

## Failure and recovery limits

A changed/unverifiable corpus before capture receives no new directory exclusion;
its contents remain eligible under the existing archive rules. A change found
after exclusion makes the export incomplete. Under the existing normal-closure
finalizer, archive failure retains the idle owned container for recovery. Keep
the full host corpus and recover changed source before removing either copy.
No uncertain add, kill, disconnect or archive is automatically replayed.

True size overflow remains incomplete. The8MiB compressed archive cap,64MiB
retained pinned-binary budget and existing credential/cache exclusions stay
unchanged. Emergency export with unknown handler/actor closure does not use
dependency directory exclusions and retains its prior best-effort limitations.
This feature does not prove descendant quiescence, continuous host immutability
or unchanged files between snapshots. Do not write to the host corpus during a
frozen arm. Missing helper output, mount drift or root replacement cannot turn
into a complete archive.

The preflight witness and two archive witnesses are outside the timed child
workload. Include their separate bounded budgets, Docker overhead and the normal
finalizers when selecting a whole-helper developer watchdog. Never shorten
finalization to the model/task deadline or retroactively change a failed arm's
original acceptance flags.
