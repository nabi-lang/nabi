// Runs wasm32-unknown-unknown modules in a real browser, through the
// shipped targets/nabi.mjs, for the corpus (NABI_TEST_WASM_BROWSER).
//
//   node browser.mjs serve chromium|firefox|webkit
//   node browser.mjs run <url> <app.wasm> [args..]
//
// `serve` launches the browser once (Playwright, from the npm prefix
// NABI_TEST_PLAYWRIGHT) and serves, on 127.0.0.1, a page, nabi.mjs,
// worker.mjs and the modules it is asked to run (as application/wasm,
// so compileStreaming takes them). It prints its URL and runs until its
// stdin closes: the harness holds that pipe, so the browser goes with
// the test process.
//
// `run` is one program run: it posts the module, its arguments, its
// environment (this process's) and stdin to the server, writes the
// program's stdout and stderr as they come and exits with its code. The
// server runs it in a fresh dedicated Worker of an idle page (a page per
// concurrent run, reused), as a web app runs a program that may block:
// the main thread cannot. The pages are not cross-origin isolated (no
// SharedArrayBuffer, nabi.mjs sleeps by spinning), as most sites are; no
// case needs shared memory. A run whose client goes away (the harness's
// timeout killed it) terminates its worker.

import { createServer } from "node:http";
import { mkdtempSync, readFileSync, readSync, rmSync, writeSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

const PAGE = `<!doctype html>
<meta charset="utf-8">
<title>nabi</title>
<script type="module">
const runs = new Map();
// One program in a fresh worker: its output to the server, in order,
// then its exit code (null when stopped).
window.start = (id, msg) =>
  new Promise((resolve) => {
    const w = new Worker("/worker.mjs", { type: "module" });
    let sent = Promise.resolve();
    const out = (fd, b64) => (sent = sent.then(() => window.nabiOut(id, fd, b64)));
    const done = (code) => {
      if (!runs.delete(id)) return;
      w.terminate();
      sent.then(() => resolve(code));
    };
    runs.set(id, done);
    w.onmessage = ({ data }) => ("exit" in data ? done(data.exit) : out(data.fd, data.b64));
    w.onerror = (e) => {
      e.preventDefault();
      out(2, btoa("worker: " + e.message + "\\n"));
      done(134);
    };
    w.postMessage(msg);
  });
window.stop = (id) => runs.get(id)?.(null);
</script>`;

const TYPES = { ".mjs": "text/javascript", ".html": "text/html" };

async function serve(name) {
  const prefix = process.env.NABI_TEST_PLAYWRIGHT;
  if (!prefix) throw new Error("NABI_TEST_PLAYWRIGHT: the npm prefix playwright is installed in");
  const pw = createRequire(join(prefix, "package.json"))("playwright");
  const kind = pw[name];
  if (!kind || !["chromium", "firefox", "webkit"].includes(name)) {
    throw new Error(`NABI_TEST_WASM_BROWSER=${name}: chromium, firefox or webkit`);
  }

  const files = {
    "/nabi.mjs": join(here, "../../targets/nabi.mjs"),
    "/worker.mjs": join(here, "worker.mjs"),
  };
  const runs = new Map();
  let next = 0;
  const server = createServer((req, res) => {
    const send = (code, type, body) => {
      res.writeHead(code, { "content-type": type, "cache-control": "no-store" });
      res.end(body);
    };
    const url = new URL(req.url, "http://x");
    const wasm = url.pathname.match(/^\/wasm\/(\d+)$/);
    if (req.method === "GET" && url.pathname === "/") return send(200, TYPES[".html"], PAGE);
    if (req.method === "GET" && files[url.pathname]) {
      return send(200, TYPES[".mjs"], readFileSync(files[url.pathname]));
    }
    if (req.method === "GET" && wasm && runs.has(wasm[1])) {
      try {
        return send(200, "application/wasm", readFileSync(runs.get(wasm[1]).wasm));
      } catch (e) {
        return send(404, "text/plain", String(e));
      }
    }
    if (req.method === "POST" && url.pathname === "/run") return run(req, res);
    send(404, "text/plain", "not found");
  });

  // Firefox's app data (profiles.ini) in a temp directory of its own:
  // the user's Firefox stays untouched, and on a Mac that does not let
  // ~/Library/Application Support/Firefox be made, it still starts.
  const data = mkdtempSync(join(tmpdir(), "nabi-browser-"));
  const browser = await kind.launch({ env: { ...process.env, MOZ_APP_DATA: data } });
  const context = await browser.newContext();
  await context.exposeFunction("nabiOut", (id, fd, b64) => {
    runs.get(id)?.res.write(JSON.stringify({ fd, b64 }) + "\n");
  });
  const idle = [];
  const page = async () => {
    const p = idle.pop();
    if (p && !p.isClosed()) return p;
    const n = await context.newPage();
    await n.goto(base);
    return n;
  };

  async function run(req, res) {
    let body = "";
    for await (const c of req) body += c;
    const msg = JSON.parse(body);
    const id = String(next++);
    res.writeHead(200, { "content-type": "application/x-ndjson" });
    runs.set(id, { res, wasm: msg.wasm });
    let p;
    let code;
    const gone = () => {
      if (code === undefined) p?.evaluate((id) => window.stop(id), id).catch(() => {});
    };
    res.on("close", gone);
    try {
      p = await page();
      if (res.destroyed) return idle.push(p);
      code = await p.evaluate(([id, m]) => window.start(id, m), [
        id,
        { wasm: `/wasm/${id}`, args: msg.args, env: msg.env, stdin: msg.stdin },
      ]);
      idle.push(p);
    } catch (e) {
      // A crashed or closed page: not reused.
      p?.close().catch(() => {});
      res.write(JSON.stringify({ fd: 2, b64: Buffer.from(`${name}: ${e.message}\n`).toString("base64") }) + "\n");
      code = 134;
    } finally {
      runs.delete(id);
    }
    if (code !== null) res.end(JSON.stringify({ exit: code }) + "\n");
  }

  await new Promise((ok) => server.listen(0, "127.0.0.1", ok));
  const base = `http://127.0.0.1:${server.address().port}/`;
  const quit = async () => {
    server.close();
    await browser.close().catch(() => {});
    rmSync(data, { recursive: true, force: true });
    process.exit(0);
  };
  process.stdin.on("end", quit).on("error", quit).resume();
  process.on("SIGTERM", quit).on("SIGINT", quit);
  process.stdout.write(`${base}\n`);
}

async function client(url, wasm, args) {
  const chunks = [];
  const buf = Buffer.alloc(65536);
  for (let n; (n = readSync(0, buf)) > 0; ) chunks.push(Buffer.from(buf.subarray(0, n)));
  const res = await fetch(new URL("run", url), {
    method: "POST",
    body: JSON.stringify({
      wasm,
      args: [wasm, ...args],
      env: process.env,
      stdin: Buffer.concat(chunks).toString("base64"),
    }),
  });
  const write = (fd, b) => {
    for (let off = 0; off < b.length; ) off += writeSync(fd, b, off);
  };
  let line = "";
  let code = 1;
  const take = (l) => {
    if (!l) return;
    const m = JSON.parse(l);
    if ("exit" in m) code = m.exit;
    else write(m.fd, Buffer.from(m.b64, "base64"));
  };
  for await (const c of res.body.pipeThrough(new TextDecoderStream())) {
    const parts = (line + c).split("\n");
    line = parts.pop();
    parts.forEach(take);
  }
  take(line);
  process.exitCode = code;
}

const [mode, ...rest] = process.argv.slice(2);
try {
  if (mode === "serve") await serve(rest[0]);
  else if (mode === "run") await client(rest[0], rest[1], rest.slice(2));
  else throw new Error("usage: browser.mjs serve <browser> | run <url> <app.wasm> [args..]");
} catch (e) {
  process.stderr.write(`browser.mjs: ${e.message}\n`);
  process.exit(1);
}
