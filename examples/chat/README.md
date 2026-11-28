# chat

A line-based TCP chat room. Clients pick a nickname, then every line
they send goes to everyone else as `<nick> text`. Joins, leaves and
nickname changes are announced.

- One task per connection, spawned on the runtime scheduler with
  `task::spawn`, suspending in `accept_async` / `read_until_async` /
  `write_all_async`.
- One hub task owns the room (a `Dict` of members). Connections send it
  `Event`s over an unbounded `channel`; each connection has a bounded
  outbox that a writer task drains, so a slow client loses lines
  instead of stalling the room. No state is shared between tasks.
- `Session` implements `Drop` and is bound with `using`: however a
  connection ends (`/quit`, end of stream, an I/O error), the hub is
  told it left.
- I/O errors are `throws (IoError)`, caught per connection; one client
  resetting its socket does not affect the others. A line cut off by a
  disconnect is dropped, not half-delivered.
- Client lines parse into a `Command` sum type and are dispatched with
  `match`.

Commands: `/who` lists who is here, `/nick NAME` renames (1 to 16 bytes,
no spaces, not taken), `/quit` leaves.

## Run

```sh
nabi run -- 127.0.0.1:7000
```

The address defaults to `127.0.0.1:7000`. The server logs connections to
stderr. Connect from other terminals:

```sh
nc 127.0.0.1 7000
```

Sample session (`>` marks what this client typed):

```
nickname?
> ada
* welcome, ada; 1 here. /who /nick NAME /quit
* bob joined
* cy joined
> hello, room
* cy is now carol
<carol> hi all
* bob left
> /who
* here: ada, carol
```

Server log:

```
listening on 127.0.0.1:7000
conn  #1 127.0.0.1:64113
join  #1 ada
conn  #2 127.0.0.1:64114
join  #2 bob
conn  #3 127.0.0.1:64115
join  #3 cy
nick  #3 cy -> carol
error #2 read: Connection reset by peer
leave #2 bob
```

Only `std::net` and `core` are used, so it runs on Linux, macOS and
Windows.
