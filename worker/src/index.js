const OBJECT_KEY = /^(packs\/[a-z0-9-]+\/[0-9a-f]{64}\.zip|covers\/[a-z0-9-]+\/[0-9a-f]{64}\.(png|jpe?g|webp))$/;

// The app sends the ticket the WeduGames license Worker signed for it (crates/core/src/license.rs). Only the public
// key is here, so this Worker needs no secret and no copy of the accounts.
const LICENSE_PUBLIC_X = "T7if60C6qOsfo8Kva71V6skY3+Yon+ikBJVJ00pkqDE=";
const LICENSE_PUBLIC_Y = "7g4MCAHWjhJiVPjG9wpV2fA09QZJDrnF0zZ5JHUG8r0=";
const TICKET_TAG = "FMPACKS27";
// The app gets a fresh ticket when it opens, before each download and every 6 hours while open, so a
// ticket copied out of it stops working within a day.
const TICKET_HOURS = 24;
const HOUR = 3_600_000;

const bytes = (b64) => Uint8Array.from(atob(b64.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));
let publicKey;

function licenseKey() {
  publicKey ??= crypto.subtle.importKey(
    "raw",
    new Uint8Array([4, ...bytes(LICENSE_PUBLIC_X), ...bytes(LICENSE_PUBLIC_Y)]),
    { name: "ECDSA", namedCurve: "P-256" },
    false,
    ["verify"],
  );
  return publicKey;
}

async function licenseIsValid(request) {
  const token = request.headers.get("Authorization")?.replace(/^Bearer\s+/i, "");
  if (!token) return false;
  let t;
  try {
    t = JSON.parse(new TextDecoder().decode(bytes(token)));
  } catch {
    return false;
  }
  const fields = [t?.hwid, t?.key, t?.issued, t?.sig, t?.expire ?? ""];
  if (!fields.every((v) => typeof v === "string" && v.length < 256)) return false;
  const text = `${TICKET_TAG}|2|${t.hwid}|${t.expire ?? ""}|${t.key}|${t.issued}`;
  let signed;
  try {
    signed = await crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, await licenseKey(), bytes(t.sig), new TextEncoder().encode(text));
  } catch {
    return false;
  }
  if (!signed) return false;
  const now = Date.now();
  // The expiry day itself is already over, as in the license Worker.
  if (t.expire && !(now < Date.parse(`${t.expire}T00:00:00Z`))) return false;
  // Only a time of issue counts: a date alone is an RTE4FM-style ticket.
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/.test(t.issued)) return false;
  const issued = Date.parse(t.issued);
  // A few minutes of slack for a server clock running ahead.
  return issued <= now + 5 * 60_000 && now - issued <= TICKET_HOURS * HOUR;
}

function serveObject(object, partial) {
  const headers = new Headers();
  object.writeHttpMetadata(headers);
  headers.set("ETag", object.httpEtag);
  headers.set("Accept-Ranges", "bytes");
  // Keys are content hashes, so an object under a key never changes.
  headers.set("Cache-Control", "private, max-age=31536000, immutable");

  if (partial) {
    const { offset, length } = object.range;
    headers.set("Content-Range", `bytes ${offset}-${offset + length - 1}/${object.size}`);
    headers.set("Content-Length", String(length));
    return new Response(object.body, { status: 206, headers });
  }
  headers.set("Content-Length", String(object.size));
  return new Response(object.body, { headers });
}

async function getObject(request, env, key) {
  const range = request.headers.get("Range");
  if (range) {
    const head = await env.BUCKET.head(key);
    if (!head) return new Response("Not found", { status: 404 });
    const from = Number(/^bytes=(\d+)-/.exec(range)?.[1]);
    if (from >= head.size) {
      return new Response("Bad range", { status: 416, headers: { "Content-Range": `bytes */${head.size}` } });
    }
  }
  const object = await env.BUCKET.get(key, range ? { range: request.headers } : {});
  return object ? serveObject(object, Boolean(range)) : new Response("Not found", { status: 404 });
}

export default {
  async fetch(request, env) {
    if (request.method !== "GET") return new Response("Method not allowed", { status: 405 });
    const open = env.REQUIRE_LICENSE !== "true";
    if (!open && !(await licenseIsValid(request))) return new Response("Unauthorized", { status: 401 });

    const { pathname } = new URL(request.url);
    if (pathname === "/manifest") {
      const manifest = await env.BUCKET.get("manifest.json");
      if (!manifest) return new Response("Not found", { status: 404 });
      return new Response(manifest.body, {
        headers: { "Content-Type": "application/json", "Cache-Control": "no-store" },
      });
    }
    if (pathname.startsWith("/objects/")) {
      const key = pathname.slice("/objects/".length);
      if (!OBJECT_KEY.test(key)) return new Response("Not found", { status: 404 });
      return getObject(request, env, key);
    }
    return new Response("Not found", { status: 404 });
  },
};
