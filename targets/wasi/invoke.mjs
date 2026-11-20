// jco.sh's `--invoke`: call an export of a component transpiled by jco
// with WAVE arguments and print its result as WAVE, as
// `wasmtime run --invoke` does.
//
//   node invoke.mjs <main.js> <component.wit> '<name>(<arg>, ...)'
//
// component.wit is `wasm-tools component wit` of the component: its
// world and every package it names. The export is a function of the
// world or of an interface it exports, found by name. The directory of
// main.js is removed once loaded.
//
// jco's JS values: s64/u64 as BigInt, records and flags as objects with
// lowerCamelCase keys, variants as {tag, val}, enums as their case's
// name, a tuple as an array, a list of numbers as a typed array,
// `option<T>` as T or undefined (an option inside an option as
// {tag, val}), and a function's `result<T, E>` as its ok value or a
// thrown error carrying the err value as `payload`; a result anywhere
// else as {tag, val}.
import { readFileSync, rmSync } from "node:fs";
import { dirname } from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

const [mainJs, witFile, call] = process.argv.slice(2);

// WIT: each interface's (and world's) types and functions.

function lex(src) {
  const toks = [];
  const re =
    /\s+|\/\/[^\n]*|\/\*[\s\S]*?\*\/|(->|[{}()<>,:;=.\/@*_])|(%?[A-Za-z][A-Za-z0-9-]*)|([0-9]+(?:\.[0-9]+)*(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?)/y;
  let m;
  while (re.lastIndex < src.length) {
    const at = re.lastIndex;
    if (!(m = re.exec(src))) throw new Error(`wit: bad input at ${at}: ${src.slice(at, at + 20)}`);
    const t = m[1] ?? m[2] ?? m[3];
    if (t !== undefined) toks.push(t.replace(/^%/, ""));
  }
  return toks;
}

// "ns:pkg/iface@ver" (or "<world>#<name>" for an interface written in
// a world) -> { types, funcs }.
const scopes = new Map();
// The world's exports: [{ scope, js }], js the path to the export's
// object in main.js (empty for the world's own functions).
const exports = [];

const newScope = (key) => {
  const s = { key, types: new Map(), funcs: new Map() };
  scopes.set(key, s);
  return s;
};

function parseWit(src) {
  const t = lex(src);
  let i = 0;
  let pkg = null;
  const peek = () => t[i];
  const next = () => t[i++];
  const eat = (s) => {
    if (t[i] !== s) throw new Error(`wit: want ${s}, got ${t[i]} at token ${i}`);
    i++;
  };
  // A balanced {...} or (...) group starting at the opener.
  const skipGroup = () => {
    const open = next();
    const close = open === "{" ? "}" : ")";
    let depth = 1;
    while (depth > 0) {
      const x = next();
      if (x === undefined) throw new Error("wit: unbalanced group");
      if (x === open) depth++;
      else if (x === close) depth--;
    }
  };
  const skipTo = (s) => {
    while (t[i] !== s) {
      if (t[i] === "{" || t[i] === "(") skipGroup();
      else i++;
    }
    i++;
  };
  // `ns:name@ver` -> { ns, name, ver }.
  const pkgId = () => {
    const ns = next();
    eat(":");
    const name = next();
    let ver = "";
    if (peek() === "@") {
      next();
      ver = next();
    }
    return { ns, name, ver };
  };
  // An interface path: `iface` of this package or `ns:pkg/iface@ver`.
  const ifaceKey = () => {
    if (t[i + 1] !== ":") return `${pkg.ns}:${pkg.name}/${next()}${pkg.ver ? `@${pkg.ver}` : ""}`;
    const ns = next();
    eat(":");
    const name = next();
    eat("/");
    const iface = next();
    let ver = "";
    if (peek() === "@") {
      next();
      ver = next();
    }
    return `${ns}:${name}/${iface}${ver ? `@${ver}` : ""}`;
  };
  function type(scope) {
    const n = next();
    const args = () => {
      eat("<");
      const xs = [];
      while (peek() !== ">") {
        xs.push(peek() === "_" ? (next(), null) : type(scope));
        if (peek() === ",") next();
      }
      eat(">");
      return xs;
    };
    switch (n) {
      case "list":
        return { k: "list", t: args()[0] };
      case "option":
        return { k: "option", t: args()[0] };
      case "tuple":
        return { k: "tuple", ts: args() };
      case "result": {
        if (peek() !== "<") return { k: "result", ok: null, err: null };
        const [ok, err] = args();
        return { k: "result", ok, err: err ?? null };
      }
      case "own":
      case "borrow":
      case "future":
      case "stream":
        if (peek() === "<") args();
        return { k: "resource" };
      default:
        return { k: "name", n, scope };
    }
  }
  // `name: type` pairs up to the closer.
  function fields(scope, close) {
    const xs = [];
    while (peek() !== close) {
      const n = next();
      eat(":");
      xs.push({ n, t: type(scope) });
      if (peek() === ",") next();
    }
    next();
    return xs;
  }
  function func(scope) {
    if (peek() === "async") next();
    eat("func");
    eat("(");
    const params = fields(scope, ")");
    let ret = null;
    if (peek() === "->") {
      next();
      ret = type(scope);
    }
    eat(";");
    return { params, ret };
  }
  // One item of an interface or world body.
  function item(scope, world) {
    const k = next();
    switch (k) {
      case "record": {
        const n = next();
        eat("{");
        scope.types.set(n, { k: "record", fs: fields(scope, "}") });
        return;
      }
      case "variant": {
        const n = next();
        eat("{");
        const cs = [];
        while (peek() !== "}") {
          const c = next();
          let ty = null;
          if (peek() === "(") {
            next();
            ty = type(scope);
            eat(")");
          }
          cs.push({ n: c, t: ty });
          if (peek() === ",") next();
        }
        next();
        scope.types.set(n, { k: "variant", cs });
        return;
      }
      case "enum":
      case "flags": {
        const n = next();
        eat("{");
        const cs = [];
        while (peek() !== "}") {
          cs.push(next());
          if (peek() === ",") next();
        }
        next();
        scope.types.set(n, { k, cs });
        return;
      }
      case "type": {
        const n = next();
        eat("=");
        scope.types.set(n, type(scope));
        eat(";");
        return;
      }
      case "resource": {
        scope.types.set(next(), { k: "resource" });
        if (peek() === "{") skipGroup();
        else eat(";");
        return;
      }
      case "use": {
        // `use path.{a, b as c};`: a and c name path's a and b.
        const key = ifaceKey();
        eat(".");
        eat("{");
        while (peek() !== "}") {
          const a = next();
          let b = a;
          if (peek() === "as") {
            next();
            b = next();
          }
          scope.types.set(b, { k: "use", key, n: a });
          if (peek() === ",") next();
        }
        next();
        eat(";");
        return;
      }
      case "import":
      case "export": {
        const out = k === "export" && world;
        if (t[i + 1] === ":" && (t[i + 2] === "func" || t[i + 2] === "async")) {
          // `export name: func(..);`
          const n = next();
          next();
          const f = func(scope);
          if (out) scope.funcs.set(n, f);
          return;
        }
        if (t[i + 1] === ":" && t[i + 2] === "interface") {
          // `export name: interface { .. }`
          const n = next();
          next();
          next();
          const s = newScope(`${world}#${n}`);
          body(s, null);
          if (out) exports.push({ scope: s, js: [camel(n)] });
          return;
        }
        // `export ns:pkg/iface@ver;` (jco exports it under that name).
        const key = ifaceKey();
        eat(";");
        if (out) exports.push({ key, js: [key] });
        return;
      }
      case "include":
        skipTo(";");
        return;
      case "@":
        // A feature gate: `@since(version = 0.2.0)`.
        next();
        if (peek() === "(") skipGroup();
        return;
      default:
        if (peek() === ":") {
          // `name: func(..);` of an interface.
          next();
          scope.funcs.set(k, func(scope));
          return;
        }
        throw new Error(`wit: unexpected ${k} at token ${i - 1}`);
    }
  }
  function body(scope, world) {
    eat("{");
    while (peek() !== "}") item(scope, world);
    next();
  }
  // Package-level: interfaces and worlds.
  function top() {
    const k = next();
    if (k === "interface") {
      const n = next();
      body(newScope(`${pkg.ns}:${pkg.name}/${n}${pkg.ver ? `@${pkg.ver}` : ""}`), null);
    } else if (k === "world") {
      const n = next();
      const s = newScope(`${pkg.ns}:${pkg.name}#${n}`);
      exports.push({ scope: s, js: [] });
      body(s, `${pkg.ns}:${pkg.name}#${n}`);
    } else if (k === "@") {
      next();
      if (peek() === "(") skipGroup();
    } else throw new Error(`wit: unexpected ${k} at token ${i - 1}`);
  }
  while (i < t.length) {
    if (next() !== "package") throw new Error(`wit: want package at token ${i - 1}`);
    const id = pkgId();
    if (peek() === ";") {
      // The root package: its items run to the next `package`.
      next();
      pkg = id;
      while (i < t.length && peek() !== "package") top();
    } else {
      const outer = pkg;
      pkg = id;
      eat("{");
      while (peek() !== "}") top();
      next();
      pkg = outer;
    }
  }
}

const PRIMS = new Set([
  "bool", "s8", "s16", "s32", "s64", "u8", "u16", "u32", "u64", "f32", "f64", "char", "string",
]);

// A type with its names (aliases, `use`s) followed to a definition.
function resolve(ty) {
  for (let seen = 0; ty && (ty.k === "name" || ty.k === "use"); seen++) {
    if (seen > 64) throw new Error(`wit: alias loop at ${ty.n}`);
    if (ty.k === "name" && PRIMS.has(ty.n)) return ty;
    const scope = ty.k === "use" ? scopes.get(ty.key) : ty.scope;
    const d = scope?.types.get(ty.n);
    if (!d) throw new Error(`wit: unknown type ${ty.n} in ${ty.k === "use" ? ty.key : scope?.key}`);
    ty = d;
  }
  return ty;
}

const camel = (s) => s.replace(/-([a-z0-9])/g, (_, c) => c.toUpperCase());
const TYPED = {
  u8: Uint8Array,
  s8: Int8Array,
  u16: Uint16Array,
  s16: Int16Array,
  u32: Uint32Array,
  s32: Int32Array,
  u64: BigUint64Array,
  s64: BigInt64Array,
  f32: Float32Array,
  f64: Float64Array,
};

// WAVE text to jco's JS value of a type.

function waveReader(src) {
  let i = 0;
  const ws = () => {
    while (/\s/.test(src[i] ?? "")) i++;
  };
  const peek = () => (ws(), src[i]);
  const eat = (c) => {
    if (peek() !== c) throw new Error(`wave: want ${c} at ${i}: ${src.slice(i)}`);
    i++;
  };
  const label = () => {
    ws();
    const m = /^%?[a-z][a-z0-9-]*/.exec(src.slice(i));
    if (!m) throw new Error(`wave: want a label at ${i}: ${src.slice(i)}`);
    i += m[0].length;
    return m[0].replace(/^%/, "");
  };
  const quoted = (q) => {
    eat(q);
    let s = "";
    for (;;) {
      const c = src[i++];
      if (c === undefined) throw new Error("wave: unterminated literal");
      if (c === q) return s;
      if (c !== "\\") {
        s += c;
        continue;
      }
      const e = src[i++];
      const esc = { n: "\n", t: "\t", r: "\r", '"': '"', "'": "'", "\\": "\\" }[e];
      if (esc !== undefined) s += esc;
      else if (e === "u") {
        const m = /^\{([0-9a-fA-F]+)\}/.exec(src.slice(i));
        if (!m) throw new Error("wave: bad \\u escape");
        s += String.fromCodePoint(parseInt(m[1], 16));
        i += m[0].length;
      } else throw new Error(`wave: bad escape \\${e}`);
    }
  };
  const number = () => {
    ws();
    const m = /^(-?inf|nan|-?[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?)/.exec(src.slice(i));
    if (!m) throw new Error(`wave: want a number at ${i}: ${src.slice(i)}`);
    i += m[0].length;
    return m[0];
  };
  // Items up to `close`, each read by `f`.
  const seq = (open, close, f) => {
    eat(open);
    const xs = [];
    while (peek() !== close) {
      xs.push(f(xs.length));
      if (peek() === ",") i++;
      else break;
    }
    eat(close);
    return xs;
  };
  function value(ty) {
    ty = resolve(ty);
    if (ty.k === "name") {
      switch (ty.n) {
        case "bool": {
          const l = label();
          if (l !== "true" && l !== "false") throw new Error(`wave: want a bool, got ${l}`);
          return l === "true";
        }
        case "s64":
        case "u64":
          return BigInt(number());
        case "f32":
        case "f64": {
          const n = number();
          return n === "nan" ? NaN : n === "inf" ? Infinity : n === "-inf" ? -Infinity : Number(n);
        }
        case "char":
          return quoted("'");
        case "string":
          return quoted('"');
        default:
          return Number(number());
      }
    }
    switch (ty.k) {
      case "list": {
        const xs = seq("[", "]", () => value(ty.t));
        const el = resolve(ty.t);
        const A = el.k === "name" && TYPED[el.n];
        return A ? A.from(xs) : xs;
      }
      case "tuple":
        return seq("(", ")", (n) => value(ty.ts[n]));
      case "record": {
        const o = {};
        seq("{", "}", () => {
          const n = label();
          eat(":");
          const f = ty.fs.find((x) => x.n === n);
          if (!f) throw new Error(`wave: no field ${n}`);
          o[camel(n)] = value(f.t);
        });
        for (const f of ty.fs) if (!(camel(f.n) in o)) o[camel(f.n)] = undefined;
        return o;
      }
      case "flags": {
        const o = Object.fromEntries(ty.cs.map((c) => [camel(c), false]));
        seq("{", "}", () => {
          o[camel(label())] = true;
        });
        return o;
      }
      case "enum":
        return label();
      case "variant": {
        const n = label();
        const c = ty.cs.find((x) => x.n === n);
        if (!c) throw new Error(`wave: no case ${n}`);
        if (!c.t) return { tag: n };
        eat("(");
        const val = value(c.t);
        eat(")");
        return { tag: n, val };
      }
      case "option": {
        const inner = resolve(ty.t).k === "option";
        const n = label();
        if (n === "none") return inner ? { tag: "none" } : undefined;
        if (n !== "some") throw new Error(`wave: want some or none, got ${n}`);
        eat("(");
        const val = value(ty.t);
        eat(")");
        return inner ? { tag: "some", val } : val;
      }
      case "result": {
        const n = label();
        const t2 = n === "ok" ? ty.ok : n === "err" ? ty.err : undefined;
        if (t2 === undefined) throw new Error(`wave: want ok or err, got ${n}`);
        if (!t2) return { tag: n };
        eat("(");
        const val = value(t2);
        eat(")");
        return { tag: n, val };
      }
      default:
        throw new Error(`wave: cannot pass a ${ty.k}`);
    }
  }
  return {
    value,
    label,
    seq,
    eat,
    end() {
      ws();
      if (i < src.length) throw new Error(`wave: trailing ${src.slice(i)}`);
    },
  };
}

// jco's JS value of a type to WAVE text.

function quote(s, q) {
  let o = q;
  for (const c of s) {
    const e = { "\n": "\\n", "\t": "\\t", "\r": "\\r", "\\": "\\\\" }[c];
    if (e) o += e;
    else if (c === q) o += "\\" + c;
    else if (c.codePointAt(0) < 0x20 || c.codePointAt(0) === 0x7f) {
      o += `\\u{${c.codePointAt(0).toString(16)}}`;
    } else o += c;
  }
  return o + q;
}

function float(x) {
  if (Number.isNaN(x)) return "nan";
  if (x === Infinity) return "inf";
  if (x === -Infinity) return "-inf";
  return String(x);
}

function wave(ty, v) {
  ty = resolve(ty);
  if (ty.k === "name") {
    switch (ty.n) {
      case "bool":
        return v ? "true" : "false";
      case "f32":
      case "f64":
        return float(v);
      case "char":
        return quote(v, "'");
      case "string":
        return quote(v, '"');
      default:
        return String(v);
    }
  }
  switch (ty.k) {
    case "list":
      return `[${Array.from(v, (x) => wave(ty.t, x)).join(", ")}]`;
    case "tuple":
      return `(${ty.ts.map((t, n) => wave(t, v[n])).join(", ")})`;
    case "record":
      return `{${ty.fs.map((f) => `${f.n}: ${wave(f.t, v[camel(f.n)])}`).join(", ")}}`;
    case "flags":
      return `{${ty.cs.filter((c) => v[camel(c)]).join(", ")}}`;
    case "enum":
      return v;
    case "variant": {
      const c = ty.cs.find((x) => x.n === v.tag);
      return c.t ? `${v.tag}(${wave(c.t, v.val)})` : v.tag;
    }
    case "option": {
      if (resolve(ty.t).k === "option") {
        return v.tag === "none" ? "none" : `some(${wave(ty.t, v.val)})`;
      }
      return v === undefined || v === null ? "none" : `some(${wave(ty.t, v)})`;
    }
    case "result": {
      const t2 = v.tag === "ok" ? ty.ok : ty.err;
      return t2 ? `${v.tag}(${wave(t2, v.val)})` : v.tag;
    }
    default:
      throw new Error(`wave: cannot print a ${ty.k}`);
  }
}

parseWit(readFileSync(witFile, "utf8"));
const r = waveReader(call);
const name = r.label();
// The world's own functions, then its exported interfaces'.
const found = [];
for (const e of exports) {
  const scope = e.scope ?? scopes.get(e.key);
  if (!scope) throw new Error(`wit: no interface ${e.key}`);
  const f = scope.funcs.get(name);
  if (f) found.push({ f, js: [...e.js, camel(name)] });
}
if (found.length !== 1) {
  throw new Error(`${found.length ? "more than one" : "no"} exported function ${name}`);
}
const { f, js } = found[0];
const args = r.seq("(", ")", (n) => {
  if (n >= f.params.length) throw new Error(`${name} takes ${f.params.length} arguments`);
  return r.value(f.params[n].t);
});
r.end();
if (args.length !== f.params.length) throw new Error(`${name} takes ${f.params.length} arguments`);

const mod = await import(pathToFileURL(mainJs).href);
rmSync(dirname(mainJs), { recursive: true, force: true });
let fn = mod;
for (const k of js) fn = fn?.[k];
if (typeof fn !== "function") throw new Error(`main.js exports no ${js.join(".")}`);
// jco unwraps a function's result: ok as the value, err thrown.
const ret = f.ret && resolve(f.ret);
let out;
if (ret && ret.k === "result") {
  try {
    out = { tag: "ok", val: await fn(...args) };
  } catch (e) {
    if (!e || !("payload" in e)) throw e;
    out = { tag: "err", val: e.payload };
  }
} else {
  out = await fn(...args);
}
if (f.ret) process.stdout.write(wave(f.ret, out) + "\n");
