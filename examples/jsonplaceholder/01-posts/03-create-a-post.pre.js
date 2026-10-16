// Sign the body, the way many webhook and payment APIs require.
const body = pm.variables.replaceIn(pm.request.body.raw);
const signature = CryptoJS.HmacSHA256(body, "demo-secret").toString();
pm.request.headers.upsert({ key: "X-Signature", value: signature });
console.log("signed body for", pm.variables.get("authorName"));
