// Usage: node scripts/new-license.mjs "Customer name" [days]
// Prints the key for the customer and the command that activates it.
import { createHash, randomBytes } from "node:crypto";

const [name = "customer", days] = process.argv.slice(2);
const key = randomBytes(16).toString("hex").match(/.{8}/g).join("-").toUpperCase();
const hash = createHash("sha256").update(key).digest("hex");
const record = { active: true, name };
if (days) record.expires = new Date(Date.now() + Number(days) * 864e5).toISOString();

console.log(`License key : ${key}`);
console.log("Activate    :");
console.log(`npx wrangler kv key put --binding=LICENSES --remote "license:${hash}" '${JSON.stringify(record)}'`);
