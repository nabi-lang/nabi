# kv-store

A persistent key-value store in one append-only log file, safe across
crashes.

It shows:

- binary records with a CRC-32 (`src/crc.nabi`), written and synced one
  at a time;
- an in-memory index (key to value offset) rebuilt by replaying the log
  on open;
- recovery: a torn last record, from a crash mid-write, is cut off;
- compaction: the live keys go to a new file that replaces the log
  atomically (`std::fs::write_atomic`: temp file, fsync, rename);
- `using` and `Drop` closing the log, a sum-type `StoreError` with
  `throws`, `try` and `catch`, and `str | none` for lookups.

## File format

The log is a sequence of records, integers little-endian:

```text
crc32: u32 | kind: u8 | key_len: u32 | value_len: u32 | key | value
```

`crc32` covers every byte after it. `kind` is 1 for a value and 2 for a
tombstone (a delete, no value). The latest record of a key wins. Opening
stops at the first record that is short or fails its checksum and cuts
the file there; since each append is synced before the next starts, only
the last record can be torn.

Compaction closes the log before replacing it, since Windows may refuse
to rename over an open file, then reopens it.

## Run

```sh
nabi run -- db.log set name nabi
nabi run -- db.log get name        # nabi
nabi run -- db.log del name
nabi run -- db.log list
nabi run -- db.log compact
nabi run -- db.log stats
nabi run -- db.log                 # commands from stdin, `quit` to end
```

`get` and `del` of a missing key exit with code 1, a bad command with 2.
In `set`, the rest of the line is the value.

`nabi test` writes logs under the temp directory, tears their last
record and checks recovery and compaction.
