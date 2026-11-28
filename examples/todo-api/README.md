# todo-api

A small JSON REST API over an in-memory todo list, built on the
[`http`](https://packages.nabi-lang.org/packages/http) and
[`json`](https://packages.nabi-lang.org/packages/json) packages.

It shows:

- typed errors: `throws (ApiError)`, `try`, and one `catch` that turns
  every failure into a status code;
- `T | none` with `??` and `if let`, including `PATCH` bodies whose
  members may be left out;
- sum types (`Route`, `ApiError`) with exhaustive `match`;
- `#[derive(json::Json)]` for request and response bodies;
- state shared safely between requests with `Mutex` and `Atomic`.

| Route                | Body                             | Success |
|----------------------|----------------------------------|---------|
| `GET /todos`         |                                  | 200     |
| `POST /todos`        | `{"title": str}`                 | 201     |
| `GET /todos/<id>`    |                                  | 200     |
| `PATCH /todos/<id>`  | `{"title": str?, "done": bool?}` | 200     |
| `DELETE /todos/<id>` |                                  | 204     |

Bad bodies get 400, unknown todos and paths 404, other methods 405, each
with a `{"error": ".."}` body.

## Run

```sh
nabi run              # listens on 127.0.0.1:8080
PORT=3000 nabi run    # or on another port
```

In PowerShell: `$env:PORT = 3000; nabi run`.

Each request is logged to stderr: method, path, status, duration.

## Try it

```console
$ curl -X POST localhost:8080/todos -d '{"title": "buy milk"}'
{"id":1,"title":"buy milk","done":false}
$ curl -X POST localhost:8080/todos -d '{"title": "walk the dog"}'
{"id":2,"title":"walk the dog","done":false}
$ curl localhost:8080/todos
[{"id":1,"title":"buy milk","done":false},{"id":2,"title":"walk the dog","done":false}]
$ curl -X PATCH localhost:8080/todos/1 -d '{"done": true}'
{"id":1,"title":"buy milk","done":true}
$ curl -X DELETE -w '%{http_code}\n' localhost:8080/todos/2
204
$ curl localhost:8080/todos/2
{"error":"not found"}
$ curl -X POST localhost:8080/todos -d '{"title": 42}'
{"error":"invalid JSON body: 1:11: expected str, found integer"}
```
