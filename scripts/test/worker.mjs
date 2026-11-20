// A dedicated worker of browser.mjs's page: runs one program through
// nabi.mjs and posts its output and then its exit code back.

import { load } from "/nabi.mjs";

const b64 = (b) => {
  let s = "";
  for (let i = 0; i < b.length; i += 32768) s += String.fromCharCode(...b.subarray(i, i + 32768));
  return btoa(s);
};
const unb64 = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));

onmessage = async ({ data: { wasm, args, env, stdin } }) => {
  const write = (fd, b) => postMessage({ fd, b64: b64(b) });
  let exit;
  try {
    const app = await load(fetch(wasm), { args, env, stdin: unb64(stdin), write });
    exit = app.main();
  } catch (e) {
    write(2, new TextEncoder().encode(`${e?.name}: ${e?.message}\n`));
    exit = 134;
  }
  postMessage({ exit });
};
