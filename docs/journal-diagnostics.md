# Read a running agent's journal

Both existing binaries provide the same local operator command:

```text
minecraft-sdk-brain journal-events /path/to/brain.sqlite AFTER [ROWS [BYTES]]
body-probe journal-events /path/to/body.sqlite AFTER [ROWS [BYTES]]
```

Use the journal in place, including its live SQLite WAL. Do not copy a live
database/checkpoint to infer a consistent current state. The command opens an
existing regular non-symlink file read-only and uses one consistent transaction.
It does not read a profile/config, acquire a controller, connect, act, infer,
migrate, chmod, checkpoint, delete sidecars, create/reset a database or poll.
SQLite's normal WAL/SHM reader coordination remains in use; this is not a claim
of zero filesystem activity. Existing OS read permissions still apply.

Defaults are 32 rows and 262,144 bytes. Limits are 1–256 rows and 4096–2097152 bytes,
including the CLI's final newline. AFTER is a nonnegative SQLite sequence within
the signed 64-bit range. Brain and body databases have separate cursors. Pass the
returned `next_cursor` as AFTER for another page; later committed appends then
become visible. The existing SQLite busy timeout is 3 seconds; a locked/wrong-schema
read fails with a nonzero exit, not an empty successful page. OS scheduling/I/O
are outside that timeout.

Each full row preserves `id`, `at`, `kind` and the exact original UTF-8 `data_json`
string, plus its SHA-256. No JSON fields are parsed, normalized or redacted. Parse
`data_json` explicitly in your own local diagnostic tools. These are historical
committed events, not a fresh Minecraft observation or a survival/completion claim.

SQLite byte LENGTH is checked before fetching raw payloads. Oversized rows get an
explicit marker with their original ID/at and original kind/data byte counts;
their omitted content/hash is unknown. Markers advance the cursor, so a large
scene cannot permanently hide later health events. To retrieve that row, retry
its preceding cursor with a larger allowed byte budget. Rows larger than the hard
limit remain explicitly omitted. A row that cannot fit after earlier full rows
instead remains eligible at `next_row_sequence`; no cursor advances past it.

`complete` means the cursor exhausted that transaction's snapshot. `data_complete`
also requires no omitted rows. Check both and `truncated_rows`; completeness of a
later page does not undo an earlier omission. At most ROWS+1 metadata records are
read. Raw payload fetches are capped at BYTES−1024 and kind text at 1024 bytes;
JSON escaping may temporarily allocate up to six times that bounded raw payload.
The serialized result itself always fits BYTES.

Rows may contain private player chat, coordinates, programs and paths. The CLI
does not redact them: keep captured output private. This is an operator CLI,
not a new model tool, gameplay API, network endpoint or arbitrary SQL interface.
